//! Thunk emission for synchronous callables, and the call-shape dispatcher
//! every callable (free function or interface member) goes through.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{AbiFn, CallShape, FnBinding};
use weaveffi_model::plan::ErrorStrategy;

use super::async_fns::gen_async_function;
use super::helpers::{
    fn_slots, ident, ret_arrow_for, sentinel, thunk_attrs, wrap_unwind, CallTarget, UserSig,
};
use super::iterators::gen_iterator_function;
use super::marshal::{build_call_body, lift_param};

/// Whether this callable routes typed domain errors through `out_err`, per
/// the plan's error contract ([`weaveffi_model::plan::ErrorStrategy`]). The
/// producer declared it by returning a `Result`, so the thunk matches on the
/// call's `Ok`/`Err` instead of binding the value directly.
pub(crate) fn throws(f: &FnBinding) -> bool {
    f.error_strategy() == ErrorStrategy::Throws
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

/// Generate the `extern "C"` thunk for one synchronous callable.
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
    let sentinel = sentinel(&abi.ret);

    // Lift each parameter, collecting the preambles and the call arguments.
    let self_pre = target.self_preamble(sentinel.as_ref(), user.receiver_is_arc());
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let (pre, arg) = lift_param(pb, user, sentinel.as_ref())?;
        preamble.extend(pre);
        call_args.push(arg);
    }

    let call = target.call(&f.name, &call_args);
    let body = build_call_body(f.ret.as_ref(), &abi.ret, throws(f), call, user)?;
    let wrapped = wrap_unwind(quote! { #self_pre #preamble #body }, sentinel.as_ref());
    let attrs = thunk_attrs();

    Ok(quote! {
        #attrs
        pub unsafe extern "C" fn #sym(#(#params),*) #arrow {
            #wrapped
        }
    })
}
