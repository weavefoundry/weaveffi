//! Thunk emission for `async fn` exports: the completion-callback typedef and
//! the launcher.
//!
//! This is the producer half of the async completion contract (see
//! [`weaveffi_model::plan`]): the callback fires exactly once, from an
//! arbitrary producer thread, and everything it delivers is *owned by the
//! consumer*. A non-null `err` is heap-boxed and released with
//! `{prefix}_error_free`; a string, bytes, typed-array, or buffered result
//! is a `(result_ptr, result_len)` run released with `{prefix}_free_bytes`;
//! an object result transfers one strong reference.
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
use weaveffi_model::plan::ResultPass;

use super::helpers::{ctype_to_rust, fn_slots, ident, slot, thunk_attrs, CallTarget, UserSig};
use super::lift::{lift_param, lower_async_result, lower_custom_ret, slot_owners, Lifts};
use super::sync::report_error;

/// Generate the completion-callback typedef and the launcher for an
/// `async fn`.
pub(crate) fn gen_async_function(
    f: &FnBinding,
    a: &AsyncBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let object = match a.result {
        ResultPass::Object { .. } => user.ret_object(),
        _ => None,
    };
    let result_slot = match &a.result {
        ResultPass::Object { result, .. } => Some(result.name.as_str()),
        _ => None,
    };
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| {
            // The result slot of an object return is spelled with the
            // producer's pointee so `super::T` stays in scope.
            match (&object, result_slot) {
                (Some(obj), Some(r)) if r == p.name => quote!(*mut #obj),
                _ => ctype_to_rust(&p.ty, prefix),
            }
        })
        .collect();
    let callback_typedef = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #cb_ty = extern "C" fn(#(#cb_slots),*);
    };

    let launch_sym = ident(&f.abi.symbol);
    let launch_params = fn_slots(&f.abi.params, &slot_owners(&f.params), user, prefix)?;

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
    let (cancel_pre, cancel) = match &a.cancel_token {
        Some(token) => {
            let token = slot(&token.name);
            lifts.args.push(quote!(__wv_cancel_arg));
            (
                quote! {
                    let __wv_cancel = ::weaveffi::abi::CancelToken::from_raw(#token);
                    let __wv_cancel_arg = ::std::clone::Clone::clone(&__wv_cancel);
                },
                quote!(::std::option::Option::Some(__wv_cancel)),
            )
        }
        None => (TokenStream::new(), quote!(::std::option::Option::None)),
    };
    let lifted = lifts.finish();
    let call = target.call(&f.name, &lifts.args);
    let output = match report_error(f, user) {
        Some(report) => quote!(#call.await.map_err(|__wv_e| #report)),
        None => quote!(::std::result::Result::Ok(#call.await)),
    };

    let val = quote!(__wv_val);
    let custom = lower_custom_ret(user, &val);
    let lowered = lower_async_result(&a.result, object.as_ref());
    let lower = match a.result {
        ResultPass::Void => quote!(move |()| ()),
        _ => quote!(move |__wv_val| { #custom #lowered }),
    };
    let slot_count = a.callback_params.len() - 2;
    let slots: Vec<syn::Ident> = (0..slot_count)
        .map(|i| ident(&format!("__wv_r{i}")))
        .collect();
    let callback = slot("callback");
    let context = slot("context");
    let attrs = thunk_attrs();

    Ok(quote! {
        #callback_typedef

        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) {
            let __wv_ctx = #context as usize;
            ::weaveffi::abi::launch_async(
                move || unsafe {
                    #lifted
                    #cancel_pre
                    ::std::result::Result::Ok((async move { #output }, #cancel))
                },
                #lower,
                move |__wv_err, (#(#slots,)*)| {
                    #callback(__wv_ctx as *mut ::std::ffi::c_void, __wv_err #(, #slots)*)
                },
            );
        }
    })
}
