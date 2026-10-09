//! Thunk emission for synchronous callables, and the call-shape dispatcher
//! every callable (free function or interface member) goes through.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use weaveffi_model::model::{AbiFn, CallShape, FnBinding};
use weaveffi_model::plan::ErrorStrategy;

use super::async_fns::gen_async_function;
use super::helpers::{
    fn_slots, ident, ret_arrow_for, ret_type_for, thunk_attrs, CallTarget, UserSig,
};
use super::iterators::gen_iterator_function;
use super::lift::{lift_param, lower_ret, Lifts};

/// Whether this callable routes typed domain errors through `out_err`, per
/// the plan's error contract ([`weaveffi_model::plan::ErrorStrategy`]). The
/// producer declared it by returning a `Result`, so the thunk propagates the
/// call's `Err` as the domain error.
pub(crate) fn throws(f: &FnBinding) -> bool {
    f.error_strategy() == ErrorStrategy::Throws
}

/// The producer call, with a throwing callable's `Err` turned into its
/// `FfiError` and propagated with `?`.
pub(crate) fn checked_call(f: &FnBinding, call: TokenStream, user: &UserSig<'_>) -> TokenStream {
    if throws(f) {
        let span = user.ret_type_span();
        quote_spanned! {span=>
            #call.map_err(|__wv_e| ::weaveffi::abi::FfiError::from_report(&__wv_e))?
        }
    } else {
        call
    }
}

/// Lift every parameter (and the receiver) of `f` for a call that borrows
/// them for its duration.
pub(crate) fn sync_lifts(
    f: &FnBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
) -> syn::Result<Lifts> {
    let mut lifts = Lifts::default();
    lifts.receiver(target.self_lift(user.receiver_is_arc()));
    for pb in &f.params {
        lift_param(&mut lifts, pb, user, false)?;
    }
    Ok(lifts)
}

/// Dispatch one callable (free function or interface member) to the codegen
/// for its call shape.
pub(crate) fn gen_function(
    f: &FnBinding,
    sig: &syn::Signature,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, target.self_ty());
    match &f.shape {
        CallShape::Sync(abi) => gen_sync_function(f, abi, &user, target, prefix),
        CallShape::Iterator(it) => gen_iterator_function(f, it, &user, target, prefix),
        CallShape::Async(a) => gen_async_function(f, a, &user, target, prefix),
    }
}

/// Generate the `extern "C"` thunk for one synchronous callable: lift every
/// input, call the producer, and lower the result, all inside
/// `weaveffi::abi::call_sync`, which catches panics and fills `out_err`.
fn gen_sync_function(
    f: &FnBinding,
    abi: &AbiFn,
    user: &UserSig<'_>,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let sym = ident(&abi.symbol);
    let params = fn_slots(&abi.params, &f.params, user, prefix)?;
    let arrow = ret_arrow_for(&abi.ret, f.ret.as_ref(), user, prefix);
    let ret_ty = ret_type_for(&abi.ret, f.ret.as_ref(), user, prefix);
    let lifts = sync_lifts(f, user, target)?;
    let lifted = lifts.finish();
    let call = checked_call(f, target.call(&f.name, &lifts.args), user);
    let lowered = match &f.ret {
        Some(ty) => lower_ret(ty, &quote!(__wv_ret), user.ret_object().as_ref()),
        None => quote!(__wv_ret),
    };
    let attrs = thunk_attrs();
    Ok(quote! {
        #attrs
        pub unsafe extern "C" fn #sym(#(#params),*) #arrow {
            unsafe {
                ::weaveffi::abi::call_sync::<#ret_ty>(out_err, move || unsafe {
                    #lifted
                    let __wv_ret = #call;
                    ::std::result::Result::Ok(#lowered)
                })
            }
        }
    })
}
