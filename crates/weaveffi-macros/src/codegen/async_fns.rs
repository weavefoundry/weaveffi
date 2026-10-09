//! Thunk emission for `async fn` exports: the completion-callback typedef and
//! the launcher.
//!
//! This is the producer half of the completion contract stated by
//! [`weaveffi_model::plan::AsyncProtocol`]: the callback fires exactly once,
//! from an arbitrary producer thread, and everything it delivers is *owned by
//! the consumer*. A non-null `err` is heap-boxed and released with
//! `{prefix}_error_free`; a string, bytes, or buffered result is a
//! `(result_ptr, result_len)` run released with `{prefix}_free_bytes`; an
//! object result transfers one strong reference.
//!
//! The launcher hands three closures to [`weaveffi::abi::launch_async`]: one
//! that lifts the inputs into owned values on the caller's thread (under
//! `catch_unwind`) and builds the producer's future, one that lowers the
//! result into the callback's slots, and one that invokes the callback. The
//! runtime owns the exactly-once guarantee: it catches panics, races a
//! `cancellable` call against its token, and completes from a drop guard if
//! the executor drops the task early.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{AsyncBinding, FnBinding};

use super::helpers::{ctype_to_rust, fn_slots, ident, thunk_attrs, CallTarget, UserSig};
use super::lift::{lift_param, lower_async_result, Lifts};
use super::sync::throws;

/// Generate the completion-callback typedef and the launcher for an
/// `async fn`.
pub(crate) fn gen_async_function(
    f: &FnBinding,
    a: &AsyncBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let is_object_ret = f.ret.as_ref().is_some_and(|t| t.interface_name().is_some());
    let object = user.ret_object();
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| {
            // The result slot of an object return is spelled with the
            // producer's pointee so `super::T` stays in scope.
            match (p.name.as_str(), &object) {
                ("result", Some(obj)) if is_object_ret => quote!(*mut #obj),
                _ => ctype_to_rust(&p.ty, prefix),
            }
        })
        .collect();
    let callback_typedef = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #cb_ty = extern "C" fn(#(#cb_slots),*);
    };

    let launch_sym = ident(&a.launch.symbol);
    let launch_params = fn_slots(&a.launch.params, &f.params, user, prefix)?;

    // Every input is owned before the future is spawned: the consumer may
    // free or reuse its arguments (and release its object references) the
    // moment the launcher returns.
    let mut lifts = Lifts::default();
    lifts.receiver(target.self_lift(true));
    for pb in &f.params {
        lift_param(&mut lifts, pb, user, true)?;
    }

    // A cancellable function takes its own reference on the token (so the
    // consumer may destroy it at any time) and hands one handle to the
    // producer, as its final parameter, and one to the runtime, which drops
    // the future and completes with the cancelled code when it fires.
    let (cancel_pre, cancel) = if f.cancellable {
        lifts.args.push(quote!(__wv_cancel_arg));
        (
            quote! {
                let __wv_cancel = ::weaveffi::abi::CancelToken::from_raw(cancel_token);
                let __wv_cancel_arg = ::std::clone::Clone::clone(&__wv_cancel);
            },
            quote!(::std::option::Option::Some(__wv_cancel)),
        )
    } else {
        (TokenStream::new(), quote!(::std::option::Option::None))
    };
    let lifted = lifts.finish();
    let call = target.call(&f.name, &lifts.args);
    let output = if throws(f) {
        quote! {
            #call.await.map_err(|__wv_e| ::weaveffi::abi::FfiError::from_report(&__wv_e))
        }
    } else {
        quote!(::std::result::Result::Ok(#call.await))
    };

    let lower = match &f.ret {
        Some(_) => {
            let lowered = lower_async_result(f.ret.as_ref(), object.as_ref());
            quote!(move |__wv_val| #lowered)
        }
        None => quote!(move |()| ()),
    };
    let slot_count = a.callback_params.len() - 2;
    let slots: Vec<syn::Ident> = (0..slot_count)
        .map(|i| ident(&format!("__wv_r{i}")))
        .collect();
    let attrs = thunk_attrs();

    Ok(quote! {
        #callback_typedef

        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) {
            let __wv_ctx = context as usize;
            ::weaveffi::abi::launch_async(
                move || unsafe {
                    #lifted
                    #cancel_pre
                    ::std::result::Result::Ok((async move { #output }, #cancel))
                },
                #lower,
                move |__wv_err, (#(#slots,)*)| {
                    callback(__wv_ctx as *mut ::std::ffi::c_void, __wv_err #(, #slots)*)
                },
            );
        }
    })
}
