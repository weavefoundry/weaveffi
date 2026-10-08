//! Emission for callback interfaces: the `#[repr(C)]` vtable struct, the
//! foreign wrapper that implements the producer's trait on top of a
//! `(ctx, vtable)` pair, and the `CallbackInterface` impl that lets thunks
//! lift `Arc<dyn Trait>` parameters.
//!
//! This is the producer half of the contract stated by
//! [`weaveffi_model::plan::CallbackProtocol`]: each method lowers its
//! arguments (strings, bytes, and buffers are borrowed for the call as
//! `(ptr, len)`; objects transfer one strong reference the consumer adopts),
//! calls the entry with `(ctx, args…, [out_ptr, out_len,] &mut err)`, adopts
//! whatever the consumer returned, and then checks `err`. Every method
//! returns `Result<T, weaveffi::ForeignError>`, so a consumer failure is an
//! `Err` and nothing unwinds.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned as _;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding};
use weaveffi_model::ty::{Family, Ty};

use super::helpers::{ident, ret_arrow_for, slot_type_for, UserSig};
use super::unsupported;

/// Build the lowering for one callback-method argument as `(preamble, C
/// arguments)`. The Rust parameter is whatever the producer's trait
/// declares; everything but an object is borrowed for the duration of the
/// vtable call.
fn lower_callback_arg(
    pb: &ParamBinding,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let n = ident(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let none = TokenStream::new();
    // Borrow the value whether it arrived owned or borrowed.
    let borrowed = if is_ref { quote!(#n) } else { quote!(&#n) };
    let ptr = ident(&format!("__wv_{}_ptr", pb.name));
    let len = ident(&format!("__wv_{}_len", pb.name));
    let pair = |slots: TokenStream| {
        (
            quote!(let (#ptr, #len) = #slots;),
            vec![quote!(#ptr), quote!(#len)],
        )
    };
    Ok(match pb.ty.family() {
        Family::Direct => match &pb.ty {
            Ty::Enum(_) => (
                none,
                vec![quote!(::weaveffi::abi::CEnum::to_i32(#borrowed))],
            ),
            _ => (none, vec![if is_ref { quote!(*#n) } else { quote!(#n) }]),
        },
        Family::String => pair(quote!(::weaveffi::abi::str_slots(&#n))),
        Family::Bytes => pair(quote!(::weaveffi::abi::byte_slots(&#n))),
        // A buffered argument is encoded into a local value buffer the
        // consumer borrows for the call.
        Family::Buffer => {
            let tmp = ident(&format!("__wv_{}_buf", pb.name));
            let value = if user.param_is_slice(&pb.name) {
                quote!(&<[_]>::to_vec(#n))
            } else {
                borrowed
            };
            (
                quote!(let #tmp = ::weaveffi::abi::encode_value(#value);),
                vec![quote!(#tmp.as_ptr()), quote!(#tmp.len())],
            )
        }
        // An object transfers one strong reference the consumer adopts (and
        // eventually `_destroy`s). The producer must hold it as an `Arc<T>`,
        // since only an `Arc` allocation can carry a reference count.
        Family::Object { nullable } => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "callback-interface object parameter that is not an `Arc<T>` (the consumer \
                     adopts a reference, so spell it `Arc<T>` or `Option<Arc<T>>`)",
                ));
            }
            let obj = user.param_object(&pb.name);
            let arg = if nullable {
                quote!(::weaveffi::abi::lower_object_opt::<#obj>(::std::clone::Clone::clone(#borrowed)))
            } else {
                quote!(::weaveffi::abi::lower_object::<#obj>(::std::sync::Arc::clone(#borrowed)))
            };
            (none, vec![arg])
        }
        Family::Callback { .. } | Family::Iterator => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "callback-interface parameter type",
            ))
        }
    })
}

/// The statements adopting the vtable entry's return (`__wv_ret`, or the
/// `out_ptr`/`out_len` run) as `__wv_value: Result<T, ForeignError>`.
fn adopt_callback_ret(ret: Option<&Ty>, user: &UserSig<'_>) -> TokenStream {
    let Some(ty) = ret else {
        return quote!(let __wv_value = ::std::result::Result::Ok(__wv_ret););
    };
    let value = match ty.family() {
        Family::Direct => match ty {
            Ty::Enum(_) => {
                let et = user.ret_object().unwrap_or_else(|| quote!(_));
                quote!(::weaveffi::abi::callback_ret_enum::<#et>(__wv_ret))
            }
            _ => quote!(::std::result::Result::Ok(__wv_ret)),
        },
        Family::Object { nullable: false } => {
            quote!(unsafe { ::weaveffi::abi::callback_ret_object(__wv_ret) })
        }
        Family::Object { nullable: true } => {
            quote!(::std::result::Result::Ok(unsafe {
                ::weaveffi::abi::callback_ret_object_opt(__wv_ret)
            }))
        }
        Family::String => {
            quote!(unsafe { ::weaveffi::abi::callback_ret_string(__wv_out_ptr, __wv_out_len) })
        }
        Family::Bytes => {
            quote!(unsafe { ::weaveffi::abi::callback_ret_bytes(__wv_out_ptr, __wv_out_len) })
        }
        Family::Buffer => {
            let ty = user.ret_value_type().unwrap_or_else(|| quote!(_));
            quote!(unsafe {
                ::weaveffi::abi::callback_ret_buffer::<#ty>(__wv_out_ptr, __wv_out_len)
            })
        }
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("validation never admits {ty} as a callback return")
        }
    };
    quote!(let __wv_value = #value;)
}

/// Emit one method of the foreign wrapper's trait impl. `domain` is the path
/// of the error domain in scope, which a `throws` method's failures are
/// checked against.
fn gen_foreign_method(
    m: &CallbackMethodBinding,
    sig: &syn::Signature,
    domain: Option<&TokenStream>,
) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, None);
    let field = ident(&m.name);
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    for pb in &m.params {
        let (p, args) = lower_callback_arg(pb, &user)?;
        pre.extend(p);
        c_args.extend(args);
    }
    let run = m
        .ret
        .as_ref()
        .is_some_and(|t| matches!(t.family(), Family::String | Family::Bytes | Family::Buffer));
    if run {
        pre.extend(quote! {
            let mut __wv_out_ptr: *mut u8 = ::std::ptr::null_mut();
            let mut __wv_out_len: usize = 0;
        });
        c_args.push(quote!(&mut __wv_out_ptr));
        c_args.push(quote!(&mut __wv_out_len));
    }
    let adopt = adopt_callback_ret(m.ret.as_ref(), &user);
    let status = match (m.throws, domain) {
        (true, Some(domain)) => {
            quote!(::weaveffi::abi::callback_status_in::<#domain>(&__wv_err)?;)
        }
        (true, None) => {
            return Err(syn::Error::new(
                sig.span(),
                "weaveffi: a #[weaveffi::throws] callback method needs a #[weaveffi::error] \
                 domain in this module or a parent module",
            ))
        }
        (false, _) => quote!(::weaveffi::abi::callback_status(&__wv_err)?;),
    };
    Ok(quote! {
        #[allow(unsafe_code, unused_variables, unused_mut, clippy::let_unit_value, clippy::unit_arg)]
        #sig {
            #pre
            let mut __wv_err = ::weaveffi::abi::FfiError::default();
            // SAFETY: the vtable and `ctx` are live for the life of the
            // `ForeignCallback`, and every argument outlives the call.
            let __wv_ret = unsafe {
                (self.0.vtable().#field)(self.0.ctx(), #(#c_args,)* &mut __wv_err)
            };
            // The consumer's return is adopted (and released on failure)
            // before its status is checked.
            #adopt
            #status
            __wv_value
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
    domain: Option<&TokenStream>,
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
        let user = UserSig::new(sig, None);
        let field = ident(&m.name);
        let slots = m
            .abi_params
            .iter()
            .map(|p| slot_type_for(p, &m.params, &user, prefix))
            .collect::<syn::Result<Vec<_>>>()?;
        let arrow = ret_arrow_for(&m.abi_ret, m.ret.as_ref(), &user, prefix);
        fields.push(quote!(pub #field: unsafe extern "C" fn(#(#slots),*) #arrow,));
        methods.push(gen_foreign_method(m, sig, domain)?);
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
