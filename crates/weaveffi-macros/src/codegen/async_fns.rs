//! Thunk emission for `async fn` exports: the completion-callback typedef and
//! the `_async` launcher.
//!
//! This is the producer half of the completion contract stated by
//! [`weaveffi_core::plan::AsyncProtocol`]: the callback fires exactly once,
//! from an arbitrary producer thread, and everything it delivers is *owned by
//! the consumer*. A non-null `err` is heap-boxed and released with
//! `weaveffi_error_free`; a string result is released with
//! `weaveffi_free_string`; a buffered or bytes result is released with
//! `weaveffi_free_bytes`; an object result transfers one strong reference.
//! This ownership transfer (unlike the borrow-and-copy contract of synchronous
//! `out_err` slots) is what lets consumers defer decoding past the callback's
//! return, which runtimes such as Dart's `NativeCallable.listener` require.
//! The callback's `err` slot follows the owning function's
//! [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy).
//!
//! The future runs on the process-wide [`weaveffi_abi::spawn`] executor (the
//! default thread-per-future spawner, or whatever the producer installed with
//! [`weaveffi_abi::set_spawner`]), wrapped in [`weaveffi_abi::CatchUnwind`]
//! so a panic is delivered through the callback rather than into the executor.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::{AsyncBinding, FnBinding, ParamBinding, Ty};

use super::helpers::{
    ctype_to_rust, fn_slots, ident, is_copy, rust_type_ident, sentinel, CallTarget, UserSig,
};
use super::sync::throws;
use super::unsupported;

/// The two "fire the callback with an error and stop" tails an async launcher
/// needs: one for the caller's thread (before the spawn, where `context` is
/// still a raw pointer and `return` leaves the launcher) and one for inside
/// the spawned future (where `__wv_ctx` is a `usize` and `return` leaves the
/// async block). Both replay the result slots as their zero/null sentinels.
struct Fail {
    sentinels: Vec<TokenStream>,
}

impl Fail {
    fn with(&self, ctx: TokenStream, build_err: TokenStream) -> TokenStream {
        let sentinels = &self.sentinels;
        quote! {{
            let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
            #build_err
            callback(
                #ctx,
                ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
                #(, #sentinels)*
            );
            return;
        }}
    }

    fn marshal(msg: &str) -> TokenStream {
        quote! {
            ::weaveffi::abi::error_set(
                &mut __wv_e,
                ::weaveffi::abi::MARSHAL_ERROR_CODE,
                #msg,
            );
        }
    }

    /// Reject on the caller's thread with a marshalling error.
    fn pre(&self, msg: &str) -> TokenStream {
        self.with(quote!(context), Self::marshal(msg))
    }

    /// Reject inside the future with a marshalling error.
    fn inner(&self, msg: &str) -> TokenStream {
        self.with(
            quote!(__wv_ctx as *mut ::std::ffi::c_void),
            Self::marshal(msg),
        )
    }
}

/// Lift one async-launcher input slot into an *owned* Rust value and give the
/// call argument the user's `async fn` receives, as `(pre_spawn, in_future,
/// arg)`.
///
/// Async inputs must own their data before the future is spawned: the foreign
/// caller may free or reuse the argument buffers as soon as the launcher
/// returns. So strings and bytes are copied and objects retained (a new strong
/// reference) on the caller's thread; a borrowed spelling (`&str`, `&T`) is
/// then satisfied by lending the owned value. There is no `out_err` slot on a
/// launcher, so an invalid input fires the completion callback with a
/// marshalling error, which keeps the "exactly once" promise.
fn lift_async_input(
    pb: &ParamBinding,
    user: &UserSig<'_>,
    fail: &Fail,
) -> syn::Result<(TokenStream, TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let none = TokenStream::new();
    let is_ref = user.param_is_ref(&pb.name);
    let arg = if is_ref {
        quote!(&#name)
    } else {
        quote!(#name)
    };
    let invalid = fail.pre(&format!("{} is null or invalid", pb.name));

    // A buffered parameter arrives as a borrowed value buffer the caller may
    // free as soon as the launcher returns: copy the raw bytes on the caller's
    // thread and decode inside the future (a malformed buffer is delivered
    // through the callback's `err` with the marshalling code).
    if pb.ty.is_buffered() {
        let ptr = ident(&format!("{}_ptr", pb.name));
        let len = ident(&format!("{}_len", pb.name));
        let decode_fail = fail.inner(&format!("{}: malformed value buffer", pb.name));
        return Ok((
            quote! {
                let #name: ::std::vec::Vec<u8> = if #ptr.is_null() {
                    ::std::vec::Vec::new()
                } else {
                    unsafe { ::std::slice::from_raw_parts(#ptr, #len) }.to_vec()
                };
            },
            quote! {
                let #name = match ::weaveffi::abi::decode_value(&#name) {
                    ::std::result::Result::Ok(__v) => __v,
                    ::std::result::Result::Err(_) => #decode_fail
                };
            },
            arg,
        ));
    }
    Ok(match &pb.ty {
        Ty::Enum(enum_name) => {
            let et = user
                .param_object(&pb.name)
                .unwrap_or_else(|| quote::ToTokens::into_token_stream(rust_type_ident(enum_name)));
            (
                quote! {
                    let #name = match <#et>::__weaveffi_from_i32(#name) {
                        ::std::option::Option::Some(__v) => __v,
                        ::std::option::Option::None => #invalid
                    };
                },
                none,
                arg,
            )
        }
        ty if is_copy(ty) => (none.clone(), none, arg),
        Ty::StringUtf8 => (
            quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => #invalid
                };
            },
            none,
            arg,
        ),
        Ty::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                none,
                arg,
            )
        }
        // An object is always retained across the spawn (the consumer may
        // release its own reference the moment the launcher returns). A `&T`
        // spelling is satisfied by lending the `Arc`, which derefs to `&T`.
        Ty::Interface(_) => {
            let arg = if user.param_wants_arc(&pb.name) {
                arg
            } else {
                quote!(&#name)
            };
            (
                quote! {
                    let #name = match unsafe { ::weaveffi::abi::object_arc(#name) } {
                        ::std::option::Option::Some(__wv_o) => __wv_o,
                        ::std::option::Option::None => #invalid
                    };
                },
                none,
                arg,
            )
        }
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            let arg = if user.param_wants_arc(&pb.name) {
                arg
            } else {
                quote!(#name.as_deref())
            };
            (
                quote!(let #name = unsafe { ::weaveffi::abi::object_arc(#name) };),
                none,
                arg,
            )
        }
        Ty::CallbackInterface(_) => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let ctx = ident(&format!("{}_ctx", pb.name));
            let vtable = ident(&format!("{}_vtable", pb.name));
            let null_fail = fail.pre(&format!("{}: null callback vtable", pb.name));
            (
                quote! {
                    let #name = match unsafe {
                        ::weaveffi::abi::lift_callback::<#dyn_ty>(#ctx, #vtable)
                    } {
                        ::std::option::Option::Some(__wv_cb) => __wv_cb,
                        ::std::option::Option::None => #null_fail
                    };
                },
                none,
                arg,
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
/// with `weaveffi_free_string`, a value or byte buffer with
/// `weaveffi_free_bytes`, and an object result is one strong reference the
/// consumer eventually releases with `_destroy`.
fn async_result_args(
    ty: &Ty,
    value: TokenStream,
    object: Option<&TokenStream>,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let none = TokenStream::new();
    let owned_bytes = |bytes: TokenStream| {
        (
            quote! {
                let __wv_res_buf = (#bytes).into_boxed_slice();
                let __wv_res_len = __wv_res_buf.len();
                let __wv_res_ptr = ::std::boxed::Box::into_raw(__wv_res_buf) as *const u8;
            },
            vec![quote!(__wv_res_ptr), quote!(__wv_res_len)],
        )
    };
    if ty.is_buffered() {
        return Ok(owned_bytes(
            quote!(::weaveffi::abi::encode_value(&(#value))),
        ));
    }
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!(let __wv_res: *mut #obj = #call;),
        None => quote!(let __wv_res = #call;),
    };
    Ok(match ty {
        Ty::Enum(_) => (none, vec![quote!((#value).__weaveffi_to_i32())]),
        t if is_copy(t) => (none, vec![value]),
        Ty::StringUtf8 => (
            quote!(let __wv_res = ::weaveffi::abi::string_to_c_ptr(&(#value));),
            vec![quote!(__wv_res)],
        ),
        Ty::Bytes => owned_bytes(value),
        Ty::Interface(_) => (
            typed(quote!(::weaveffi::abi::lower_object(#value))),
            vec![quote!(__wv_res)],
        ),
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => (
            typed(quote!(::weaveffi::abi::lower_object_opt(#value))),
            vec![quote!(__wv_res)],
        ),
        _ => return Err(unsupported("async return", "result type")),
    })
}

/// Generate the completion-callback typedef and the `_async` launcher for an
/// `async fn`.
///
/// The launcher lifts inputs into owned values on the caller's thread, then
/// hands a `Send + 'static` future to [`weaveffi_abi::spawn`]. The future
/// awaits the producer's `async fn` under [`weaveffi_abi::CatchUnwind`] and
/// invokes the host's callback with `(context, err, result…)` exactly once: a
/// `Result` return routes its `Err` through a boxed `weaveffi_error`, a panic
/// (or a consumer callback's failure) is reported with the reserved code, and
/// success passes a null `err`. The foreign caller's `context` is carried as a
/// `usize` so the future is `Send`; the callback pointer is `Send` on its own.
pub(crate) fn gen_async_function(
    f: &FnBinding,
    a: &AsyncBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
) -> syn::Result<TokenStream> {
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| {
            // The result slot of an object return is spelled with the
            // producer's pointee so `super::T` stays in scope.
            let is_object_ret = f.ret.as_ref().is_some_and(|t| t.interface_name().is_some());
            match (p.name.as_str(), user.ret_object()) {
                ("result", Some(obj)) if is_object_ret => quote!(*mut #obj),
                _ => ctype_to_rust(&p.ty),
            }
        })
        .collect();
    let callback_typedef = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #cb_ty = extern "C" fn(#(#cb_slots),*);
    };

    let launch_sym = ident(&a.launch.symbol);
    let launch_params = fn_slots(&a.launch.params, &f.params, user)?;

    let fail = Fail {
        sentinels: a
            .callback_params
            .iter()
            .skip(2)
            .map(|p| sentinel(&p.ty))
            .collect(),
    };

    // Lift each logical input into three parts: a pre-spawn statement that runs
    // on the caller's thread (owning borrowed data, retaining objects), an
    // in-future statement that finishes the lift (decoding a value buffer),
    // and the argument forwarded to the producer.
    let mut pre_spawn = TokenStream::new();
    let mut in_future = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();

    // An async method retains its receiver for the life of the call: the
    // consumer may release its own reference the moment the launcher returns.
    // A null receiver still fires the callback (with an error), so the
    // continuation never hangs.
    if let CallTarget::Method(ty) = target {
        let null_self = fail.pre("self is null");
        pre_spawn.extend(quote! {
            let __wv_obj = match unsafe { ::weaveffi::abi::object_arc::<#ty>(__wv_self) } {
                ::std::option::Option::Some(__wv_o) => __wv_o,
                ::std::option::Option::None => #null_self
            };
        });
    }

    for pb in &f.params {
        let (pre, inc, arg) = lift_async_input(pb, user, &fail)?;
        pre_spawn.extend(pre);
        in_future.extend(inc);
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
    let object = user.ret_object();
    let (result_pre, success_args) = match &f.ret {
        Some(ty) => async_result_args(ty, quote!(__wv_val), object.as_ref())?,
        None => (TokenStream::new(), Vec::new()),
    };
    let success_call = quote! {
        #result_pre
        callback(__wv_ctx as *mut ::std::ffi::c_void, ::std::ptr::null_mut() #(, #success_args)*);
    };
    let sentinels = &fail.sentinels;

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
                    callback(
                        __wv_ctx as *mut ::std::ffi::c_void,
                        ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
                        #(, #sentinels)*
                    );
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
            ::weaveffi::abi::spawn(async move {
                // The inner future finishes lifting, awaits the producer, and
                // fires the callback. A panic anywhere inside (including a
                // consumer callback-interface failure surfacing as a
                // `ForeignError`) is caught and delivered through `err`, so
                // the continuation fires exactly once. A failure the producer
                // recorded instead of unwinding (`panic = "abort"` builds) is
                // taken before the success callback can fire, for the same
                // reason.
                let __wv_run = ::weaveffi::abi::CatchUnwind::new(async move {
                    #in_future
                    let __wv_out = #call.await;
                    match ::weaveffi::abi::take_foreign_error() {
                        ::std::option::Option::Some(__wv_foreign) => {
                            let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                            ::weaveffi::abi::error_set(
                                &mut __wv_e,
                                __wv_foreign.code,
                                &__wv_foreign.message,
                            );
                            callback(
                                __wv_ctx as *mut ::std::ffi::c_void,
                                ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
                                #(, #sentinels)*
                            );
                        }
                        ::std::option::Option::None => {
                            #dispatch
                        }
                    }
                })
                .await;
                if let ::std::result::Result::Err(__wv_panic) = __wv_run {
                    let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                    ::weaveffi::abi::error_set_panic(&mut __wv_e, &*__wv_panic);
                    callback(
                        __wv_ctx as *mut ::std::ffi::c_void,
                        ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_e))
                        #(, #sentinels)*
                    );
                }
            });
        }
    })
}
