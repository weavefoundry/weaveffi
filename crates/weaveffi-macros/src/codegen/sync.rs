//! Thunk emission for synchronous callables, and the call-shape dispatcher
//! every callable (free function or interface member) goes through.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::abi::CType;
use weaveffi_core::model::{AbiFn, CallShape, FnBinding};
use weaveffi_core::plan::ErrorStrategy;

use super::async_fns::gen_async_function;
use super::helpers::{fn_slots, ident, ret_arrow_for, sentinel, wrap_unwind, CallTarget};
use super::iterators::gen_iterator_function;
use super::marshal::{build_call_body, lift_param};

/// Whether this callable routes typed domain errors through `out_err`, per
/// the plan's error contract ([`weaveffi_core::plan::ErrorStrategy`]). The
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
) -> syn::Result<TokenStream> {
    match &f.shape {
        CallShape::Sync(abi) => gen_sync_function(f, abi, sig, target),
        CallShape::Iterator(it) => gen_iterator_function(f, it, sig, target),
        CallShape::Async(a) => gen_async_function(f, a, sig, target),
    }
}

/// Generate the `extern "C"` thunk for one synchronous callable.
fn gen_sync_function(
    f: &FnBinding,
    abi: &AbiFn,
    sig: &syn::Signature,
    target: &CallTarget,
) -> syn::Result<TokenStream> {
    let sym = ident(&abi.symbol);
    let params: Vec<TokenStream> = fn_slots(&abi.params, f, sig);
    let arrow = ret_arrow_for(&abi.ret, f, sig);
    let sentinel = sentinel(&abi.ret);

    // Lift each parameter, collecting the preambles and the call arguments.
    let self_pre = target.self_preamble(&sentinel);
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let is_ref = super::helpers::param_is_ref(sig, &pb.name);
        let (pre, arg) = lift_param(pb, is_ref, &sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }

    let call = target.call(&f.name, &call_args);
    let body = build_call_body(&f.ret, &abi.ret, throws(f), call)?;
    let is_void = matches!(abi.ret, CType::Void);
    let wrapped = wrap_unwind(
        quote! { #self_pre #preamble #body },
        (!is_void).then_some(&sentinel),
    );

    Ok(quote! {
        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #sym(#(#params),*) #arrow {
            #wrapped
        }
    })
}
