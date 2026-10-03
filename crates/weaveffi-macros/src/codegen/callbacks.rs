//! Emission for callback interfaces: the `#[repr(C)]` vtable struct, the
//! foreign wrapper that implements the producer's trait on top of a
//! `(ctx, vtable)` pair, and the `CallbackInterface` impl that lets thunks
//! lift `Arc<dyn Trait>` parameters.
//!
//! This is the producer half of the contract stated by
//! [`weaveffi_model::plan::CallbackProtocol`]: each method lowers its
//! arguments (strings, bytes, and buffers are borrowed for the call as
//! `(ptr, len)`; objects transfer one strong reference the consumer adopts),
//! calls the entry with `(ctx, args…, &mut err)`, and then checks `err`. A
//! method declared to return `Result<T, weaveffi::ForeignError>` returns the
//! consumer's failure as an `Err`; one returning a plain `T` raises it
//! through `weaveffi::abi::raise_foreign_error` (an unwind on unwinding
//! builds, a deferred report to the enclosing thunk on `panic = "abort"`
//! builds).

use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use syn::spanned::Spanned as _;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding, Ty};

use super::helpers::{ident, is_copy, ret_arrow, rust_type_ident, slot_type_for, UserSig};
use super::unsupported;

/// Build the lowering for one callback-method argument as `(preamble, C
/// arguments)`. The Rust parameter is whatever the producer's trait
/// declares; everything is borrowed for the duration of the vtable call.
fn lower_callback_arg(
    pb: &ParamBinding,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let n = ident(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let none = TokenStream::new();
    // Borrow the value whether it arrived owned or borrowed.
    let borrowed = if is_ref { quote!(#n) } else { quote!(&#n) };
    let tmp = ident(&format!("__wv_cb_{}", pb.name));

    // A buffered payload is borrowed for the call: encode it into a local
    // value buffer, hand the consumer its `(ptr, len)` view, and let the
    // encoding drop afterward.
    if pb.ty.is_buffered() {
        return Ok((
            quote!(let #tmp = ::weaveffi::abi::encode_value(#borrowed);),
            vec![quote!(#tmp.as_ptr()), quote!(#tmp.len())],
        ));
    }
    Ok(match &pb.ty {
        Ty::Enum(_) => (none, vec![quote!(#n.__weaveffi_to_i32())]),
        ty if is_copy(ty) => {
            let v = if is_ref { quote!(*#n) } else { quote!(#n) };
            (none, vec![v])
        }
        Ty::StringUtf8 => (
            quote!(let #tmp: &str = ::std::convert::AsRef::<str>::as_ref(#borrowed);),
            vec![quote!(#tmp.as_ptr()), quote!(#tmp.len())],
        ),
        Ty::Bytes => (
            quote!(let #tmp: &[u8] = ::std::convert::AsRef::<[u8]>::as_ref(#borrowed);),
            vec![quote!(#tmp.as_ptr()), quote!(#tmp.len())],
        ),
        // An object transfers one strong reference the consumer adopts (and
        // eventually `_destroy`s). The producer must hold it as an `Arc<T>`,
        // since only an `Arc` allocation can carry a reference count.
        Ty::Interface(_) => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "callback-interface object parameter that is not an `Arc<T>` (the consumer \
                     adopts a reference, so spell it `Arc<T>`)",
                ));
            }
            let obj = user.param_object(&pb.name);
            (
                none,
                vec![
                    quote!(::weaveffi::abi::lower_object::<#obj>(::std::sync::Arc::clone(#borrowed))),
                ],
            )
        }
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "callback-interface object parameter that is not an `Option<Arc<T>>`",
                ));
            }
            let obj = user.param_object(&pb.name);
            (
                none,
                vec![
                    quote!(::weaveffi::abi::lower_object_opt::<#obj>(::std::clone::Clone::clone(#borrowed))),
                ],
            )
        }
        _ => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "callback-interface parameter type",
            ))
        }
    })
}

/// The lift of a successful vtable return `__wv_ret` into the method's value
/// type, as `(lift, fallback)`. `lift` evaluates to
/// `Result<T, ForeignError>` (an out-of-range enum discriminant is a
/// marshalling failure); `fallback` is the value a plain-`T` method returns
/// after raising a failure on a `panic = "abort"` build. Callback returns are
/// direct-family only.
fn lift_callback_ret(
    ret: Option<&Ty>,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, TokenStream)> {
    Ok(match ret {
        None => (quote!(::std::result::Result::Ok(())), TokenStream::new()),
        Some(Ty::Enum(name)) => {
            let et = user
                .ret_object()
                .unwrap_or_else(|| rust_type_ident(name).into_token_stream());
            (
                quote! {
                    <#et>::__weaveffi_from_i32(__wv_ret).ok_or_else(|| {
                        ::weaveffi::abi::ForeignError {
                            code: ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            message: ::std::string::String::from(
                                "callback interface returned an invalid enum discriminant",
                            ),
                        }
                    })
                },
                quote!(<#et>::__weaveffi_placeholder()),
            )
        }
        Some(ty) if is_copy(ty) => (
            quote!(::std::result::Result::Ok(__wv_ret)),
            quote!(__wv_ret),
        ),
        Some(_) => {
            return Err(unsupported(
                user.ret_span(),
                "callback return",
                "non-direct return type",
            ))
        }
    })
}

/// Emit one method of the foreign wrapper's trait impl.
fn gen_foreign_method(m: &CallbackMethodBinding, sig: &syn::Signature) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, None);
    let field = ident(&m.name);
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    for pb in &m.params {
        let (p, args) = lower_callback_arg(pb, &user)?;
        pre.extend(p);
        c_args.extend(args);
    }
    let (lift, fallback) = lift_callback_ret(m.ret.as_ref(), &user)?;
    let result = quote! {
        ::weaveffi::abi::foreign_status(&__wv_err).and_then(|()| #lift)
    };
    let tail = if user.returns_result() {
        result
    } else {
        quote! {
            match #result {
                ::std::result::Result::Ok(__wv_v) => __wv_v,
                ::std::result::Result::Err(__wv_e) => {
                    ::weaveffi::abi::raise_foreign_error(__wv_e);
                    #fallback
                }
            }
        }
    };
    Ok(quote! {
        #[allow(unsafe_code, unused_variables, clippy::let_unit_value, clippy::unit_arg)]
        #sig {
            #pre
            let mut __wv_err = ::weaveffi::abi::FfiError::default();
            // SAFETY: the vtable and `ctx` are live for the life of the
            // `ForeignCallback`, and every argument outlives the call.
            let __wv_ret = unsafe {
                (self.0.vtable().#field)(self.0.ctx(), #(#c_args,)* &mut __wv_err)
            };
            #tail
        }
    })
}

/// Emit everything a callback interface needs on the producer side.
///
/// * `{vtable_tag}`: the `#[repr(C)]` vtable struct, one `unsafe extern "C"`
///   function pointer per method in declaration order plus the trailing
///   `free`, exactly as the generated header declares it.
/// * `__WeaveffiForeign_{Trait}`: a newtype over
///   [`ForeignCallback`](weaveffi_abi::ForeignCallback) implementing the
///   producer's trait by forwarding each call through the vtable.
/// * `impl CallbackInterface for dyn Trait`, which is how a thunk elsewhere
///   in the crate (possibly a nested module) names the vtable type and lifts
///   the `(ctx, vtable)` pair into an `Arc<dyn Trait>`.
pub(crate) fn gen_callback_interface(
    cb: &CallbackInterfaceBinding,
    item: &syn::ItemTrait,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let vt = ident(&cb.vtable_tag);
    let trait_ident = ident(&cb.name);
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
        let arrow = ret_arrow(&m.abi_ret, prefix);
        fields.push(quote!(pub #field: unsafe extern "C" fn(#(#slots),*) #arrow,));
        methods.push(gen_foreign_method(m, sig)?);
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
            #(#fields)*
            pub free: unsafe extern "C" fn(ctx: *mut ::std::ffi::c_void),
        }

        impl ::weaveffi::abi::Vtable for #vt {
            fn free(&self) -> unsafe extern "C" fn(*mut ::std::ffi::c_void) {
                self.free
            }
        }

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
