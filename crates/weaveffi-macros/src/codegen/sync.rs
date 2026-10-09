//! Thunk emission for synchronous callables, and the call-shape dispatcher
//! every callable (free function or interface member) goes through.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use weaveffi_model::model::{CallShape, FnBinding};
use weaveffi_model::plan::{ErrorStrategy, RetPass};

use super::async_fns::gen_async_function;
use super::custom::CustomScope;
use super::helpers::{
    fn_slots, ident, ret_arrow_for, ret_type_for, thunk_attrs, CallTarget, UserSig,
};
use super::iterators::gen_iterator_function;
use super::lift::{lift_param, lower_custom_ret, lower_ret, slot_owners, Lifts};

/// The expression mapping the producer's `Err(e)` (bound to `__wv_e`) to the
/// `FfiError` the callable reports, per its error strategy: a declared
/// domain's own code, message, and payload, or (`throws any`) the generic
/// code with `e`'s `Display`. `None` for a callable that doesn't throw.
///
/// The conversion is spanned on the producer's error type, so a missing
/// `Display` (for `throws any`) is reported there.
pub(crate) fn report_error(f: &FnBinding, user: &UserSig<'_>) -> Option<TokenStream> {
    let span = user
        .ret_error_spelled()
        .map_or_else(|| user.ret_type_span(), |(_, span)| span);
    match &f.error {
        ErrorStrategy::Trap => None,
        ErrorStrategy::Domain(_) => Some(quote_spanned! {span=>
            ::weaveffi::abi::FfiError::from_domain(&__wv_e)
        }),
        ErrorStrategy::Untyped => Some(quote_spanned! {span=>
            ::weaveffi::abi::FfiError::untyped(&__wv_e)
        }),
    }
}

/// The producer call, with a throwing callable's `Err` turned into its
/// `FfiError` and propagated with `?`.
pub(crate) fn checked_call(f: &FnBinding, call: TokenStream, user: &UserSig<'_>) -> TokenStream {
    match report_error(f, user) {
        Some(report) => quote!(#call.map_err(|__wv_e| #report)?),
        None => call,
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
    customs: CustomScope<'_>,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let user = UserSig::new(sig, target.self_ty(), customs);
    match (&f.shape, &f.ret_pass) {
        (CallShape::Async(a), _) => gen_async_function(f, a, &user, target, prefix),
        (CallShape::Sync, RetPass::Iterator(it)) => {
            gen_iterator_function(f, it, &user, target, prefix)
        }
        (CallShape::Sync, _) => gen_sync_function(f, &user, target, prefix),
    }
}

/// Generate the `extern "C"` thunk for one synchronous callable: lift every
/// input, call the producer, and lower the result, all inside
/// `weaveffi::abi::call_sync`, which catches panics and fills `out_err`.
fn gen_sync_function(
    f: &FnBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let sym = ident(&f.abi.symbol);
    let params = fn_slots(&f.abi.params, &slot_owners(&f.params), user, prefix)?;
    let object_ret = matches!(f.ret_pass, RetPass::Object { .. });
    let arrow = ret_arrow_for(&f.abi.ret, object_ret, user, prefix);
    let ret_ty = ret_type_for(&f.abi.ret, object_ret, user, prefix);
    let lifts = sync_lifts(f, user, target)?;
    let lifted = lifts.finish();
    let call = checked_call(f, target.call(&f.name, &lifts.args), user);
    let ret = quote!(__wv_ret);
    let custom = lower_custom_ret(user, &ret);
    let lowered = lower_ret(&f.ret_pass, &ret, user.ret_object().as_ref());
    let attrs = thunk_attrs();
    Ok(quote! {
        #attrs
        pub unsafe extern "C" fn #sym(#(#params),*) #arrow {
            unsafe {
                ::weaveffi::abi::call_sync::<#ret_ty>(__wv_out_err, move || unsafe {
                    #lifted
                    let __wv_ret = #call;
                    #custom
                    ::std::result::Result::Ok(#lowered)
                })
            }
        }
    })
}
