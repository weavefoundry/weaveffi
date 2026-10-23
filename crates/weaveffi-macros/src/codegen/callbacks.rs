//! Emission for callback interfaces: the `#[repr(C)]` vtable struct, the
//! foreign wrapper that implements the producer's trait on top of a
//! `(ctx, vtable)` pair, and the `CallbackInterface` impl that lets thunks
//! lift `Arc<dyn Trait>` parameters.
//!
//! This is the producer half of the callback contract (see
//! [`weaveffi_model::plan`]): each method lowers its arguments as their
//! [`ArgPass`] names them (strings, bytes, arrays, and buffers are borrowed
//! for the call; objects transfer one strong reference the consumer adopts),
//! calls the entry with `(ctx, args…, [return out slots,] &mut err)`, adopts
//! whatever the consumer returned (as its [`CallbackRetPass`] says), and
//! then checks `err`. A method returns `Result<T, E>` with
//! `E: From<ForeignError>`, so a consumer failure is an `Err` and nothing
//! unwinds; when the method throws a declared domain, the consumer's codes
//! of that domain arrive as its typed variants.
//!
//! A method that returns a value first checks
//! [`ForeignCallback::check_thread`](weaveffi::abi::ForeignCallback::check_thread),
//! so a thread-affine vtable's value-returning methods fail (without being
//! called) off the thread that passed the vtable.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned as _;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, CallbackParamBinding,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};

use super::custom::{CustomScope, LiftSite};
use super::helpers::{
    ctype_to_rust, ident, pointee, ret_arrow_for, slot_type_for, SlotOwners, UserSig,
};
use super::unsupported;

/// The object slots of a callback method's parameters.
fn slot_owners(params: &[CallbackParamBinding]) -> SlotOwners<'_> {
    SlotOwners {
        objects: params
            .iter()
            .filter_map(|p| match &p.pass {
                ArgPass::Object { slot, .. } => Some((p.name.as_str(), slot.name.as_str())),
                _ => None,
            })
            .collect(),
        vtables: Vec::new(),
    }
}

/// Build the lowering for one callback-method argument as `(preamble, C
/// arguments)`. The Rust parameter is whatever the producer's trait
/// declares; everything but an object is borrowed for the duration of the
/// vtable call.
fn lower_callback_arg(
    pb: &CallbackParamBinding,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let n = ident(&pb.name);
    let mut pre = TokenStream::new();
    // Borrow the value whether it arrived owned or borrowed. A value whose
    // type mentions a custom type is first converted to its repr.
    let (borrowed, is_slice) = match user.param_shape(&pb.name) {
        Some(shape) => {
            let repr = ident(&format!("__wv_{}_repr", pb.name));
            let source = if user.param_is_ref(&pb.name) {
                quote!(#n)
            } else {
                quote!(&#n)
            };
            let converted = shape.lower(source);
            pre.extend(quote!(let #repr = #converted;));
            (quote!(&#repr), false)
        }
        None if user.param_is_ref(&pb.name) => (quote!(#n), user.param_is_slice(&pb.name)),
        None => (quote!(&#n), false),
    };
    let tmp = |suffix: &str| ident(&format!("__wv_{}_{suffix}", pb.name));
    let args = match &pb.pass {
        ArgPass::Direct { .. } => vec![quote!(::weaveffi::abi::Scalar::to_abi(#borrowed))],
        ArgPass::OptDirect { .. } => {
            let (has, value) = (tmp("has"), tmp("value"));
            pre.extend(quote! {
                let (#has, #value) =
                    ::weaveffi::abi::opt_slots(::std::option::Option::as_ref(#borrowed));
            });
            vec![quote!(#has), quote!(#value)]
        }
        ArgPass::Slice { .. } => {
            let arr = tmp("arr");
            pre.extend(quote! {
                let #arr = ::weaveffi::abi::Scalar::abi_slice(&(#borrowed)[..]);
            });
            vec![quote!(#arr.as_ptr()), quote!(#arr.len())]
        }
        ArgPass::String { .. } => {
            let text = tmp("text");
            pre.extend(quote!(let #text = ::weaveffi::abi::Text::as_text(#borrowed);));
            vec![quote!(#text.as_ptr()), quote!(#text.len())]
        }
        ArgPass::Bytes { .. } => {
            let (ptr, len) = (tmp("ptr"), tmp("len"));
            pre.extend(quote!(let (#ptr, #len) = ::weaveffi::abi::byte_slots(#borrowed);));
            vec![quote!(#ptr), quote!(#len)]
        }
        // A buffered argument is encoded into a local value buffer the
        // consumer borrows for the call.
        ArgPass::Buffer { .. } => {
            let buf = tmp("buf");
            let value = if is_slice {
                quote!(&<[_]>::to_vec(#borrowed))
            } else {
                borrowed
            };
            pre.extend(quote!(let #buf = ::weaveffi::abi::encode_value(#value);));
            vec![quote!(#buf.as_ptr()), quote!(#buf.len())]
        }
        // An object transfers one strong reference the consumer adopts (and
        // eventually `_destroy`s). The producer must hold it as an `Arc<T>`,
        // since only an `Arc` allocation can carry a reference count.
        ArgPass::Object { nullable, .. } => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "callback-interface object parameter that is not an `Arc<T>` (the consumer \
                     adopts a reference, so spell it `Arc<T>` or `Option<Arc<T>>`)",
                ));
            }
            let obj = user.param_object(&pb.name);
            vec![if *nullable {
                quote!(::weaveffi::abi::lower_object_opt::<#obj>(::std::clone::Clone::clone(#borrowed)))
            } else {
                quote!(::weaveffi::abi::lower_object::<#obj>(::std::sync::Arc::clone(#borrowed)))
            }]
        }
        ArgPass::Callback { .. } => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "callback-interface parameter type",
            ))
        }
    };
    Ok((pre, args))
}

/// The out-slot locals a method's return needs (declared before the call),
/// the C arguments pointing at them, and the expression adopting the return
/// (`__wv_ret` plus the out slots) as a `Result<_, ForeignError>`.
fn callback_ret(
    m: &CallbackMethodBinding,
    prefix: &str,
) -> (TokenStream, Vec<TokenStream>, TokenStream) {
    let local = |name: &str| ident(&format!("__wv_{name}"));
    match &m.ret_pass {
        CallbackRetPass::Void => (
            TokenStream::new(),
            vec![],
            quote!(::std::result::Result::Ok(__wv_ret)),
        ),
        CallbackRetPass::Direct => (
            TokenStream::new(),
            vec![],
            quote!(::weaveffi::abi::callback_ret_scalar(__wv_ret)),
        ),
        CallbackRetPass::OptDirect { out_value } => {
            let v = local(&out_value.name);
            let ty = ctype_to_rust(pointee(&out_value.ty), prefix);
            (
                quote!(let mut #v: #ty = ::weaveffi::abi::Sentinel::sentinel();),
                vec![quote!(&mut #v)],
                quote!(::weaveffi::abi::callback_ret_opt(__wv_ret, #v)),
            )
        }
        CallbackRetPass::Slice {
            out_ptr, out_len, ..
        } => {
            let (p, l) = (local(&out_ptr.name), local(&out_len.name));
            let ty = ctype_to_rust(pointee(&out_ptr.ty), prefix);
            (
                quote! {
                    let mut #p: #ty = ::std::ptr::null_mut();
                    let mut #l: usize = 0;
                },
                vec![quote!(&mut #p), quote!(&mut #l)],
                quote!(unsafe { ::weaveffi::abi::callback_ret_slice(#p, #l) }),
            )
        }
        CallbackRetPass::String { out_ptr, out_len }
        | CallbackRetPass::Bytes { out_ptr, out_len }
        | CallbackRetPass::Buffer { out_ptr, out_len } => {
            let (p, l) = (local(&out_ptr.name), local(&out_len.name));
            let f = match &m.ret_pass {
                CallbackRetPass::String { .. } => quote!(callback_ret_text),
                CallbackRetPass::Bytes { .. } => quote!(callback_ret_bytes),
                _ => quote!(callback_ret_buffer),
            };
            (
                quote! {
                    let mut #p: *mut u8 = ::std::ptr::null_mut();
                    let mut #l: usize = 0;
                },
                vec![quote!(&mut #p), quote!(&mut #l)],
                quote!(unsafe { ::weaveffi::abi::#f(#p, #l) }),
            )
        }
        CallbackRetPass::Object {
            nullable: false, ..
        } => (
            TokenStream::new(),
            vec![],
            quote!(unsafe { ::weaveffi::abi::callback_ret_object(__wv_ret) }),
        ),
        CallbackRetPass::Object { nullable: true, .. } => (
            TokenStream::new(),
            vec![],
            quote!(::std::result::Result::Ok(unsafe {
                ::weaveffi::abi::callback_ret_object_opt(__wv_ret)
            })),
        ),
    }
}

/// Emit one method of the foreign wrapper's trait impl.
fn gen_foreign_method(
    m: &CallbackMethodBinding,
    sig: &syn::Signature,
    customs: CustomScope<'_>,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, None, customs);
    let field = ident(&m.name);
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    for pb in &m.params {
        let (p, args) = lower_callback_arg(pb, &user)?;
        pre.extend(p);
        c_args.extend(args);
    }
    let (ret_pre, ret_args, adopt) = callback_ret(m, prefix);
    pre.extend(ret_pre);
    c_args.extend(ret_args);

    let (err_ty, err_span) = user
        .ret_error_spelled()
        .unwrap_or_else(|| (quote!(::weaveffi::ForeignError), sig.span()));
    let status = match &m.error {
        ErrorStrategy::Domain(_) => quote_spanned! {err_span=>
            ::weaveffi::abi::callback_status_in::<#err_ty>(&__wv_err)?;
        },
        ErrorStrategy::Untyped | ErrorStrategy::Trap => quote_spanned! {err_span=>
            ::weaveffi::abi::callback_status::<#err_ty>(&__wv_err)?;
        },
    };
    let lowered_ty = user.ret_lowered_type().unwrap_or_else(|| quote!(()));
    let custom = match user.ret_shape() {
        Some(shape) => {
            let convert = shape.lift(quote!(__wv_repr), LiftSite::Returned);
            quote!(let __wv_value = __wv_value.and_then(|__wv_repr| #convert);)
        }
        None => TokenStream::new(),
    };
    let returns_value = !matches!(m.abi.ret, CType::Void) || !m.ret_pass.out_slots().is_empty();
    // Every conversion into the producer's error type is spanned on it, so
    // a missing `From<ForeignError>` is reported there.
    let from = quote_spanned!(err_span=> ::weaveffi::abi::convert_foreign::<#err_ty>);
    let thread_check = if returns_value {
        quote! {
            if let ::std::result::Result::Err(__wv_e) = self.0.check_thread() {
                return ::std::result::Result::Err(#from(__wv_e));
            }
        }
    } else {
        TokenStream::new()
    };
    Ok(quote! {
        #[allow(
            unsafe_code,
            unused_variables,
            unused_mut,
            clippy::let_unit_value,
            clippy::unit_arg,
            clippy::useless_conversion
        )]
        #sig {
            #thread_check
            #pre
            let mut __wv_err = ::weaveffi::abi::FfiError::default();
            // SAFETY: the vtable and `ctx` are live for the life of the
            // `ForeignCallback`, and every argument outlives the call.
            let __wv_ret = unsafe {
                (self.0.vtable().#field)(self.0.ctx(), #(#c_args,)* &mut __wv_err)
            };
            // The consumer's return is adopted (and released on failure)
            // before its status is checked.
            let __wv_value: ::std::result::Result<#lowered_ty, ::weaveffi::ForeignError> = #adopt;
            #status
            #custom
            __wv_value.map_err(|__wv_e| #from(__wv_e))
        }
    })
}

/// Emit everything a callback interface needs on the producer side.
///
/// * `{vtable_tag}`: the `#[repr(C)]` vtable struct, the runtime's
///   `VtableHeader` (`size`, `flags`, `free`) followed by one
///   `unsafe extern "C"` function pointer per method in declaration order,
///   exactly as the generated header declares it.
/// * `__WeaveffiForeign_{Trait}`: a newtype over
///   [`ForeignCallback`](weaveffi::abi::ForeignCallback) implementing the
///   producer's trait by forwarding each call through the vtable.
/// * `impl CallbackInterface for dyn Trait`, which is how a thunk elsewhere
///   in the crate (possibly a nested module) names the vtable type and lifts
///   the `(ctx, vtable)` pair into an `Arc<dyn Trait>`.
pub(crate) fn gen_callback_interface(
    cb: &CallbackInterfaceBinding,
    item: &syn::ItemTrait,
    customs: CustomScope<'_>,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let vt = ident(&cb.vtable_tag);
    let trait_ident = &item.ident;
    let foreign = ident(&format!("__WeaveffiForeign_{}", cb.name));

    let mut fields: Vec<TokenStream> = Vec::new();
    let mut methods: Vec<TokenStream> = Vec::new();
    for m in &cb.methods {
        let sig = item
            .items
            .iter()
            .find_map(|ti| match ti {
                syn::TraitItem::Fn(f) if f.sig.ident == m.name => Some(&f.sig),
                _ => None,
            })
            .ok_or_else(|| {
                syn::Error::new(
                    item.span(),
                    format!(
                        "internal error: no source for callback method `{}::{}`",
                        cb.name, m.name
                    ),
                )
            })?;
        let user = UserSig::new(sig, None, customs);
        let field = ident(&m.name);
        let owners = slot_owners(&m.params);
        let slots = m
            .abi
            .params
            .iter()
            .map(|p| slot_type_for(p, &owners, &user, prefix))
            .collect::<syn::Result<Vec<_>>>()?;
        let object_ret = matches!(m.ret_pass, CallbackRetPass::Object { .. });
        let arrow = ret_arrow_for(&m.abi.ret, object_ret, &user, prefix);
        fields.push(quote!(pub #field: unsafe extern "C" fn(#(#slots),*) #arrow,));
        methods.push(gen_foreign_method(m, sig, customs, prefix)?);
    }

    let vtable_doc = format!(
        "The C vtable a consumer supplies to implement the `{}` callback interface.",
        cb.name
    );
    Ok(quote! {
        #[doc = #vtable_doc]
        #[doc(hidden)]
        #[repr(C)]
        #[allow(non_camel_case_types, non_snake_case)]
        pub struct #vt {
            pub header: ::weaveffi::abi::VtableHeader,
            #(#fields)*
        }

        // SAFETY: `#[repr(C)]` with the header as its first field.
        #[allow(unsafe_code)]
        unsafe impl ::weaveffi::abi::Vtable for #vt {}

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub struct #foreign(::weaveffi::abi::ForeignCallback<#vt>);

        #[allow(deprecated)]
        impl #trait_ident for #foreign {
            #(#methods)*
        }

        impl ::weaveffi::abi::CallbackInterface for dyn #trait_ident {
            type Vtable = #vt;
            fn from_foreign(
                cb: ::weaveffi::abi::ForeignCallback<#vt>,
            ) -> ::std::sync::Arc<Self> {
                ::std::sync::Arc::new(#foreign(cb))
            }
        }
    })
}
