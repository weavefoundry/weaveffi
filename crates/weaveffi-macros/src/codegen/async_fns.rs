//! Thunk emission for `async fn` exports: the completion-callback typedef and
//! the `_async` launcher.
//!
//! This is the producer half of the completion contract stated by
//! [`weaveffi_core::plan::AsyncProtocol`]: the callback fires exactly once,
//! from an arbitrary producer thread, and everything it delivers is *owned by
//! the consumer*. A non-null `err` is heap-boxed and released with
//! `weaveffi_error_free`; a string result is released with
//! `weaveffi_free_string`; a buffered result is released with
//! `weaveffi_free_bytes`; an owned-object result transfers ownership. This
//! ownership transfer (unlike the borrow-and-copy contract of synchronous
//! `out_err` slots) is what lets consumers defer decoding past the callback's
//! return, which runtimes such as Dart's `NativeCallable.listener` require.
//! The callback's `err` slot follows the owning function's
//! [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy).

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{AsyncBinding, FnBinding, ParamBinding};

use super::helpers::{
    ctype_to_rust, fn_slots, ident, is_copy, sentinel, user_param_type, CallTarget,
};
use super::sync::throws;
use super::unsupported;

/// Lift one async-launcher input slot into an *owned* Rust value and give the
/// call argument the user's `async fn` receives.
///
/// Async inputs must own their data before the worker thread runs: the foreign
/// caller may free or reuse the argument buffers as soon as the launcher
/// returns. So borrowed slots (`&str`, `&[u8]`, `[&str]`) are copied here, just
/// like the hand-written async sample does. There is no `out_err` slot on a
/// launcher, so an invalid input is lifted leniently (an unreadable string
/// becomes empty) rather than reported - the completion callback is the only
/// error channel, and it fires with the result the future produces.
fn lift_async_input(
    pb: &ParamBinding,
    sig: &syn::Signature,
) -> syn::Result<(TokenStream, TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let none = TokenStream::new();
    // A buffered parameter arrives as a borrowed value buffer the caller may
    // free as soon as the launcher returns: copy the raw bytes on the
    // caller's thread, decode on the worker thread (inside its catch_unwind,
    // so a malformed buffer is delivered through the callback's `err` with
    // the panic code rather than unwinding across the C boundary).
    if pb.ty.is_buffered() {
        let ptr = ident(&format!("{}_ptr", pb.name));
        let len = ident(&format!("{}_len", pb.name));
        let panic_msg = format!("{}: malformed WeaveFFI value buffer", pb.name);
        return Ok((
            quote! {
                let #name: ::std::vec::Vec<u8> = if #ptr.is_null() {
                    ::std::vec::Vec::new()
                } else {
                    unsafe { ::std::slice::from_raw_parts(#ptr, #len) }.to_vec()
                };
            },
            quote! {
                let #name = ::weaveffi::abi::decode_value(&#name).expect(#panic_msg);
            },
            quote!(#name),
        ));
    }
    Ok(match &pb.ty {
        ty if is_copy(ty) && !matches!(ty, Ty::Enum(_)) => (none.clone(), none, quote!(#name)),
        // A typed handle is an opaque pointer, which is not `Send`. Carry it
        // across the worker-thread boundary as a `usize` and rebuild it inside
        // the closure with the producer's own pointer type (so a `super::T`
        // path stays in scope), mirroring the hand-written async sample.
        Ty::TypedHandle(_) => {
            let addr = ident(&format!("__wv_addr_{}", pb.name));
            let uty = user_param_type(sig, &pb.name)
                .ok_or_else(|| unsupported(&pb.name, "async typed-handle parameter"))?;
            (
                quote!(let #addr = #name as usize;),
                quote!(let #name = #addr as #uty;),
                quote!(#name),
            )
        }
        Ty::StringUtf8 => (
            quote!(let #name = ::weaveffi::abi::c_ptr_to_string(#name).unwrap_or_default();),
            none,
            quote!(#name),
        ),
        Ty::BorrowedStr => (
            quote!(let #name = ::weaveffi::abi::c_ptr_to_string(#name).unwrap_or_default();),
            none,
            quote!(&#name),
        ),
        Ty::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                none,
                quote!(#name),
            )
        }
        Ty::BorrowedBytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                none,
                quote!(&#name),
            )
        }
        _ => return Err(unsupported(&pb.name, "async parameter type")),
    })
}

/// Lower the future's output into the completion callback's *result* arguments
/// (the slots after `context` and `err`), returning `(preamble, args)`.
///
/// Every result transfers ownership to the consumer, per the plan's async
/// contract ([`weaveffi_core::plan::AsyncProtocol`]): a string is released
/// with `weaveffi_free_string`, a value buffer with `weaveffi_free_bytes`,
/// and an owned interface result is adopted (the consumer eventually calls
/// `_destroy`). Ownership transfer is what lets consumers defer decoding past
/// the callback's return. `bytes` results are not yet supported.
fn async_result_args(ty: &Ty, value: TokenStream) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let none = TokenStream::new();
    // A buffered result crosses as an owned `(ptr, len)` pair the consumer
    // decodes at its leisure and releases with `weaveffi_free_bytes`.
    if ty.is_buffered() {
        return Ok((
            quote! {
                let __wv_res_buf = ::weaveffi::abi::encode_value(&(#value)).into_boxed_slice();
                let __wv_res_len = __wv_res_buf.len();
                let __wv_res_ptr = ::std::boxed::Box::into_raw(__wv_res_buf) as *const u8;
            },
            vec![quote!(__wv_res_ptr), quote!(__wv_res_len)],
        ));
    }
    Ok(match ty {
        t if is_copy(t) && !matches!(t, Ty::Enum(_)) => (none.clone(), vec![value]),
        Ty::Enum(_) => (none.clone(), vec![quote!((#value) as i32)]),
        // An owned string the consumer releases with `weaveffi_free_string`.
        Ty::StringUtf8 | Ty::BorrowedStr => (
            quote!(let __wv_res = ::weaveffi::abi::string_to_c_ptr(&(#value));),
            vec![quote!(__wv_res)],
        ),
        // An owned-object result: the callback adopts the pointer (the plan's
        // `result_adopt`) and the consumer eventually calls `_destroy`.
        Ty::Interface(_) => (
            none.clone(),
            vec![quote!(::std::boxed::Box::into_raw(::std::boxed::Box::new(#value)))],
        ),
        _ => return Err(unsupported("async return", "result type")),
    })
}

/// Generate the completion-callback typedef and the `_async` launcher for an
/// `async fn`.
///
/// The launcher lifts inputs into owned values, spawns a worker thread, drives
/// the future to completion with `weaveffi::abi::block_on`, then invokes the
/// host's callback with `(context, err, result…)`. A `Result`
/// return routes its `Err` through a transient `weaveffi_error`; success passes
/// a null `err`. The foreign caller's `context` is moved across the thread
/// boundary as a `usize` (so the closure is `Send`); the callback pointer is
/// `Send` on its own.
pub(crate) fn gen_async_function(
    f: &FnBinding,
    a: &AsyncBinding,
    sig: &syn::Signature,
    target: &CallTarget,
) -> syn::Result<TokenStream> {
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| ctype_to_rust(&p.ty))
        .collect();
    let callback_typedef = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #cb_ty = extern "C" fn(#(#cb_slots),*);
    };

    let launch_sym = ident(&a.launch.symbol);
    let launch_params: Vec<TokenStream> = fn_slots(&a.launch.params, f, sig);

    // The error path replays the result slots as their zero/null sentinels.
    let sentinels: Vec<TokenStream> = a
        .callback_params
        .iter()
        .skip(2)
        .map(|p| sentinel(&p.ty))
        .collect();

    // Lift each logical input into three parts: a pre-spawn statement that runs
    // on the caller's thread (owning borrowed data, bouncing a non-`Send`
    // handle through a `usize`), an in-closure statement that reconstitutes the
    // value on the worker thread, and the argument forwarded to the producer.
    let mut pre_spawn = TokenStream::new();
    let mut in_closure = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();

    // An async method's receiver crosses the worker-thread boundary the same
    // way a typed handle does: as a `usize`, rebuilt into `&T` inside the
    // closure. The object outlives the call by the ABI contract (the consumer
    // must not destroy it while a call is in flight). A null receiver still
    // fires the callback (with an error), so the continuation never hangs.
    if let CallTarget::Method(ty) = target {
        pre_spawn.extend(quote! {
            if __wv_self.is_null() {
                let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                ::weaveffi::abi::error_set(
                    &mut __wv_e,
                    ::weaveffi::abi::MARSHAL_ERROR_CODE,
                    "self is null",
                );
                callback(
                    context,
                    ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
                    #(, #sentinels)*
                );
                return;
            }
            let __wv_self_addr = __wv_self as usize;
        });
        in_closure.extend(quote! {
            let __wv_obj: &#ty = unsafe { &*(__wv_self_addr as *const #ty) };
        });
    }

    for pb in &f.params {
        let (pre, inc, arg) = lift_async_input(pb, sig)?;
        pre_spawn.extend(pre);
        in_closure.extend(inc);
        call_args.push(arg);
    }

    // A `#[weaveffi::cancellable]` function receives the launcher's
    // `cancel_token` slot as a `Send` [`CancelToken`] appended after its
    // logical inputs (the producer declares it as the final parameter).
    // Building it before the spawn keeps the raw pointer off the capture list.
    if f.cancellable {
        pre_spawn.extend(quote! {
            let __wv_cancel = ::weaveffi::CancelToken::from_raw(cancel_token);
        });
        call_args.push(quote!(__wv_cancel));
    }

    let is_throws = throws(f);
    let (result_pre, success_args) = match &f.ret {
        Some(ty) => async_result_args(ty, quote!(__wv_val))?,
        None => (TokenStream::new(), Vec::new()),
    };
    // Result buffers and strings transfer ownership to the consumer, which
    // releases them with the runtime free symbols, per the async plan
    // contract.
    let success_call = quote! {
        #result_pre
        callback(__wv_ctx as *mut ::std::ffi::c_void, ::std::ptr::null_mut() #(, #success_args)*);
    };

    // A non-null `err` is heap-boxed: the consumer copies what it needs and
    // releases it with `weaveffi_error_free`.
    let fail_call = quote! {
        callback(
            __wv_ctx as *mut ::std::ffi::c_void,
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
            #(, #sentinels)*
        );
    };

    let dispatch = if is_throws {
        let bind = if f.ret.is_some() {
            quote!(__wv_val)
        } else {
            quote!(_)
        };
        quote! {
            match __wv_out {
                ::std::result::Result::Ok(#bind) => { #success_call }
                ::std::result::Result::Err(__wv_err) => {
                    let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                    ::weaveffi::abi::error_set_with_payload(
                        &mut __wv_e,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                        ::weaveffi::abi::ErrorReport::payload(&__wv_err),
                    );
                    #fail_call
                }
            }
        }
    } else if f.ret.is_some() {
        quote! {
            let __wv_val = __wv_out;
            #success_call
        }
    } else {
        quote! {
            let () = __wv_out;
            #success_call
        }
    };

    let call = target.call(&f.name, &call_args);

    Ok(quote! {
        #callback_typedef

        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #launch_sym(#(#launch_params),*) {
            #pre_spawn
            let __wv_ctx = context as usize;
            // Drive the future to completion, then fire the host callback. On a
            // threaded target this happens on a worker thread so the launcher
            // returns immediately; `wasm32` has no threads, so run inline (the
            // sample futures are ready without awaiting real I/O). A panic in
            // the producer's future is caught and delivered through the
            // callback's `err` argument with the reserved panic code, so the
            // consumer's continuation always fires exactly once.
            let __wv_body = move || {
                let __wv_run = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    #in_closure
                    let __wv_out = ::weaveffi::abi::block_on(#call);
                    #dispatch
                }));
                if let ::std::result::Result::Err(__wv_panic) = __wv_run {
                    let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                    ::weaveffi::abi::error_set_panic(&mut __wv_e, &*__wv_panic);
                    #fail_call
                }
            };
            #[cfg(target_arch = "wasm32")]
            {
                __wv_body();
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                ::std::thread::spawn(__wv_body);
            }
        }
    })
}
