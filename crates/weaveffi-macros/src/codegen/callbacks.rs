//! Emission for callback interfaces: the `#[repr(C)]` vtable struct, the
//! foreign wrapper that implements the producer's trait on top of a
//! `(ctx, vtable)` pair, and the `CallbackInterface` impl that lets thunks
//! lift `Arc<dyn Trait>` parameters.
//!
//! This is the producer half of the contract stated by
//! [`weaveffi_core::plan::CallbackProtocol`]: each method lowers its
//! arguments (strings and buffers are borrowed for the call and freed after
//! the vtable entry returns; objects transfer one strong reference the
//! consumer adopts), calls the entry with `(ctx, args…, &mut err)`, and then
//! checks `err`. A consumer failure is raised through
//! `weaveffi::abi::raise_foreign_error`: on unwinding builds it unwinds with a
//! `ForeignError` payload that the enclosing thunk reports as
//! `FOREIGN_ERROR_CODE`; on `panic = "abort"` builds it is recorded and the
//! thunk picks it up after the producer returns.

use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use weaveffi_core::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding, Ty};

use super::helpers::{ident, is_copy, ret_arrow, rust_type_ident, slot_type_for, UserSig};
use super::unsupported;

/// Build the lowering for one callback-method argument as `(preamble, C
/// arguments, cleanup)`. The Rust parameter is whatever the producer's trait
/// declares; `is_ref` says whether it arrives borrowed.
fn lower_callback_arg(
    pb: &ParamBinding,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, Vec<TokenStream>, TokenStream)> {
    let n = ident(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let none = TokenStream::new();
    // Borrow the value for encoding whether it arrived owned or borrowed.
    let borrowed = if is_ref { quote!(#n) } else { quote!(&#n) };

    // A buffered payload is borrowed for the call: encode it into a local
    // value buffer, hand the consumer its `(ptr, len)` view, and let the
    // encoding drop afterward.
    if pb.ty.is_buffered() {
        let tmp = ident(&format!("__wv_cb_{}", pb.name));
        return Ok((
            quote!(let #tmp = ::weaveffi::abi::encode_value(#borrowed);),
            vec![quote!(#tmp.as_ptr()), quote!(#tmp.len())],
            none,
        ));
    }
    Ok(match &pb.ty {
        Ty::Enum(_) => (none.clone(), vec![quote!(#n.__weaveffi_to_i32())], none),
        ty if is_copy(ty) => {
            let v = if is_ref { quote!(*#n) } else { quote!(#n) };
            (none.clone(), vec![v], none)
        }
        Ty::StringUtf8 => {
            let tmp = ident(&format!("__wv_cb_{}", pb.name));
            (
                quote!(let #tmp = ::weaveffi::abi::string_to_c_ptr(&#n);),
                vec![quote!(#tmp)],
                quote!(::weaveffi::abi::free_string(#tmp);),
            )
        }
        Ty::Bytes => (
            none.clone(),
            vec![quote!(#n.as_ptr()), quote!(#n.len())],
            none,
        ),
        // An object transfers one strong reference the consumer adopts (and
        // eventually `_destroy`s). The producer must hold it as an `Arc<T>`,
        // since only an `Arc` allocation can carry a reference count.
        Ty::Interface(_) => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    &pb.name,
                    "callback-interface object parameter that is not an `Arc<T>` (the consumer \
                     adopts a reference, so spell it `Arc<T>`)",
                ));
            }
            let obj = user.param_object(&pb.name);
            (
                none.clone(),
                vec![
                    quote!(::weaveffi::abi::lower_object::<#obj>(::std::sync::Arc::clone(#borrowed))),
                ],
                none,
            )
        }
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            if !user.param_wants_arc(&pb.name) {
                return Err(unsupported(
                    &pb.name,
                    "callback-interface object parameter that is not an `Option<Arc<T>>`",
                ));
            }
            let obj = user.param_object(&pb.name);
            (
                none.clone(),
                vec![quote!(::weaveffi::abi::lower_object_opt::<#obj>(#n.clone()))],
                none,
            )
        }
        _ => return Err(unsupported(&pb.name, "callback-interface parameter type")),
    })
}

/// The expression that lifts a vtable entry's return value `__wv_ret` into
/// the trait method's return type. Callback returns are direct-family only.
fn lift_callback_ret(ret: Option<&Ty>, user: &UserSig<'_>) -> syn::Result<TokenStream> {
    Ok(match ret {
        None => TokenStream::new(),
        Some(Ty::Enum(name)) => {
            let et = user
                .ret_object()
                .unwrap_or_else(|| rust_type_ident(name).into_token_stream());
            quote! {
                match <#et>::__weaveffi_from_i32(__wv_ret) {
                    ::std::option::Option::Some(__wv_e) => __wv_e,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::raise_foreign_error(::weaveffi::abi::ForeignError {
                            code: ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            message: ::std::string::String::from(
                                "callback interface returned an invalid enum discriminant",
                            ),
                        });
                        <#et>::__weaveffi_placeholder()
                    }
                }
            }
        }
        Some(ty) if is_copy(ty) => quote!(__wv_ret),
        Some(_) => return Err(unsupported("callback return", "non-direct return type")),
    })
}

/// Emit one method of the foreign wrapper's trait impl.
fn gen_foreign_method(m: &CallbackMethodBinding, sig: &syn::Signature) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, None);
    let field = ident(&m.name);
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    let mut cleanup = TokenStream::new();
    for pb in &m.params {
        let (p, args, free) = lower_callback_arg(pb, &user)?;
        pre.extend(p);
        c_args.extend(args);
        cleanup.extend(free);
    }
    let lifted = lift_callback_ret(m.ret.as_ref(), &user)?;
    Ok(quote! {
        #[allow(unsafe_code, unused_variables, clippy::let_unit_value)]
        #sig {
            #pre
            let mut __wv_err = ::weaveffi::abi::weaveffi_error::default();
            let __wv_ret = unsafe {
                (self.0.vtable().#field)(self.0.ctx(), #(#c_args,)* &mut __wv_err)
            };
            #cleanup
            ::weaveffi::abi::check_foreign_error(__wv_err);
            #lifted
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
                    Span::call_site(),
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
            .map(|p| slot_type_for(p, &m.params, &user))
            .collect::<syn::Result<Vec<_>>>()?;
        let arrow = ret_arrow(&m.abi_ret);
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
