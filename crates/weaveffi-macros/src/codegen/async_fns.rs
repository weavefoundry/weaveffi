//! Thunk emission for `async fn` exports: the completion-callback typedef and
//! the launcher.
//!
//! This is the producer half of the completion contract stated by
//! [`weaveffi_model::plan::AsyncProtocol`]: the callback fires exactly once,
//! from an arbitrary producer thread, and everything it delivers is *owned by
//! the consumer*. A non-null `err` is heap-boxed and released with
//! `{prefix}_error_free`; a string, bytes, or buffered result is a
//! `(result_ptr, result_len)` run released with `{prefix}_free_bytes`; an
//! object result transfers one strong reference. This ownership transfer
//! (unlike the borrow-and-copy contract of synchronous `out_err` slots) is
//! what lets consumers defer decoding past the callback's return, which
//! runtimes such as Dart's `NativeCallable.listener` require.
//!
//! The launcher lifts its inputs on the caller's thread and hands the rest to
//! [`weaveffi_abi::run_async`], which owns the exactly-once guarantee: it
//! catches panics, races a `cancellable` call against its token (dropping the
//! producer's future and completing with the cancelled code when the token
//! fires), and completes with the cancelled code from a drop guard if the
//! executor drops the task early.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{AsyncBinding, FnBinding, ParamBinding, Ty};

use super::helpers::{
    ctype_to_rust, fn_slots, ident, is_copy, rust_type_ident, sentinel, thunk_attrs, CallTarget,
    UserSig,
};
use super::sync::throws;
use super::unsupported;

/// The "fire the callback with a marshalling error and leave the launcher"
/// tail for an input rejected on the caller's thread.
fn reject_pre(msg: &str, sentinels: &[TokenStream]) -> TokenStream {
    quote! {{
        callback(
            context,
            ::weaveffi::abi::boxed_error(::weaveffi::abi::FfiError::new(
                ::weaveffi::abi::MARSHAL_ERROR_CODE,
                #msg,
            ))
            #(, #sentinels)*
        );
        return;
    }}
}

/// The `return Err(..)` tail for an input rejected inside the future.
fn reject_in_future(msg: &str) -> TokenStream {
    quote! {
        return ::std::result::Result::Err(::weaveffi::abi::FfiError::new(
            ::weaveffi::abi::MARSHAL_ERROR_CODE,
            #msg,
        ))
    }
}

/// Lift one async-launcher input into an *owned* Rust value, as `(pre_spawn,
/// in_future, arg)`.
///
/// Async inputs must own their data before the future is spawned: the
/// consumer may free or reuse the argument buffers as soon as the launcher
/// returns. So strings and bytes are copied and objects retained (a new
/// strong reference) on the caller's thread; a borrowed spelling (`&str`,
/// `&T`) is then satisfied by lending the owned value. There is no `out_err`
/// slot on a launcher, so an invalid input fires the completion callback with
/// a marshalling error, which keeps the "exactly once" promise.
fn lift_async_input(
    pb: &ParamBinding,
    user: &UserSig<'_>,
    sentinels: &[TokenStream],
) -> syn::Result<(TokenStream, TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let none = TokenStream::new();
    let arg = if user.param_is_ref(&pb.name) {
        quote!(&#name)
    } else {
        quote!(#name)
    };
    let invalid = reject_pre(&format!("{} is null or invalid", pb.name), sentinels);
    let ptr = ident(&format!("{}_ptr", pb.name));
    let len = ident(&format!("{}_len", pb.name));

    // A buffered parameter is copied on the caller's thread and decoded
    // inside the future (a malformed buffer is delivered through the
    // callback's `err` with the marshalling code).
    if pb.ty.is_buffered() {
        let decode_fail = reject_in_future(&format!("{}: malformed value buffer", pb.name));
        return Ok((
            quote! {
                let #name = match ::weaveffi::abi::lift_bytes(#ptr, #len) {
                    ::std::option::Option::Some(__wv_v) => __wv_v,
                    ::std::option::Option::None => #invalid
                };
            },
            quote! {
                let #name = match ::weaveffi::abi::decode_value(&#name) {
                    ::std::result::Result::Ok(__wv_v) => __wv_v,
                    ::std::result::Result::Err(_) => { #decode_fail }
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
                        ::std::option::Option::Some(__wv_v) => __wv_v,
                        ::std::option::Option::None => #invalid
                    };
                },
                none,
                arg,
            )
        }
        ty if is_copy(ty) => (none.clone(), none, arg),
        Ty::StringUtf8 | Ty::Bytes => {
            let lift = if matches!(pb.ty, Ty::StringUtf8) {
                quote!(lift_string)
            } else {
                quote!(lift_bytes)
            };
            (
                quote! {
                    let #name = match ::weaveffi::abi::#lift(#ptr, #len) {
                        ::std::option::Option::Some(__wv_v) => __wv_v,
                        ::std::option::Option::None => #invalid
                    };
                },
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
                    let #name = match ::weaveffi::abi::object_arc(#name) {
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
                quote!(let #name = ::weaveffi::abi::object_arc(#name);),
                none,
                arg,
            )
        }
        Ty::CallbackInterface(_) => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let ctx = ident(&format!("{}_ctx", pb.name));
            let vtable = ident(&format!("{}_vtable", pb.name));
            let null_fail = reject_pre(&format!("{}: null callback vtable", pb.name), sentinels);
            (
                quote! {
                    let #name = match ::weaveffi::abi::lift_callback::<#dyn_ty>(#ctx, #vtable) {
                        ::std::option::Option::Some(__wv_cb) => __wv_cb,
                        ::std::option::Option::None => #null_fail
                    };
                },
                none,
                arg,
            )
        }
        _ => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "async parameter type",
            ))
        }
    })
}

/// The expression lowering the future's output `__wv_val` into the
/// completion callback's *result* arguments (the slots after `context` and
/// `err`), as a tuple. Every result transfers ownership to the consumer.
fn async_result_slots(
    ty: &Ty,
    object: Option<&TokenStream>,
    user: &UserSig<'_>,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let pair = |bytes: TokenStream| {
        (
            quote!(let (__wv_r0, __wv_r1) = ::weaveffi::abi::bytes_into_raw(#bytes);),
            vec![quote!(__wv_r0), quote!(__wv_r1)],
        )
    };
    if ty.is_buffered() {
        return Ok(pair(quote!(::weaveffi::abi::encode_value(&__wv_val))));
    }
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!(let __wv_r0: *mut #obj = #call;),
        None => quote!(let __wv_r0 = #call;),
    };
    let one = vec![quote!(__wv_r0)];
    Ok(match ty {
        Ty::Enum(_) => (quote!(let __wv_r0 = __wv_val.__weaveffi_to_i32();), one),
        t if is_copy(t) => (quote!(let __wv_r0 = __wv_val;), one),
        Ty::StringUtf8 => pair(quote! {
            ::std::string::String::into_bytes(::std::convert::Into::into(__wv_val))
        }),
        Ty::Bytes => pair(quote!(::std::convert::Into::<::std::vec::Vec<u8>>::into(
            __wv_val
        ))),
        Ty::Interface(_) => (typed(quote!(::weaveffi::abi::lower_object(__wv_val))), one),
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => (
            typed(quote!(::weaveffi::abi::lower_object_opt(__wv_val))),
            one,
        ),
        _ => return Err(unsupported(user.ret_span(), "async return", "result type")),
    })
}

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
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| {
            // The result slot of an object return is spelled with the
            // producer's pointee so `super::T` stays in scope.
            match (p.name.as_str(), user.ret_object()) {
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
    let sentinels: Vec<TokenStream> = a
        .callback_params
        .iter()
        .skip(2)
        .filter_map(|p| sentinel(&p.ty))
        .collect();

    // Lift each input into three parts: a pre-spawn statement on the
    // caller's thread (owning borrowed data, retaining objects), an
    // in-future statement that finishes the lift (decoding a value buffer),
    // and the argument forwarded to the producer.
    let mut pre_spawn = TokenStream::new();
    let mut in_future = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();

    // An async method retains its receiver for the life of the call: the
    // consumer may release its own reference the moment the launcher returns.
    if let CallTarget::Method(ty) = target {
        let null_self = reject_pre("self is null", &sentinels);
        pre_spawn.extend(quote! {
            let __wv_obj = match ::weaveffi::abi::object_arc::<#ty>(__wv_self) {
                ::std::option::Option::Some(__wv_o) => __wv_o,
                ::std::option::Option::None => #null_self
            };
        });
    }

    for pb in &f.params {
        let (pre, inc, arg) = lift_async_input(pb, user, &sentinels)?;
        pre_spawn.extend(pre);
        in_future.extend(inc);
        call_args.push(arg);
    }

    // A cancellable function takes its own reference on the token (so the
    // consumer may destroy it at any time) and hands one handle to the
    // producer, as its final parameter, and one to the runtime, which drops
    // the future and completes with the cancelled code when it fires.
    let cancel = if f.cancellable {
        pre_spawn.extend(quote! {
            let __wv_cancel = ::weaveffi::abi::CancelToken::from_raw(cancel_token);
            let __wv_cancel_arg = ::std::clone::Clone::clone(&__wv_cancel);
        });
        call_args.push(quote!(__wv_cancel_arg));
        quote!(::std::option::Option::Some(__wv_cancel))
    } else {
        quote!(::std::option::Option::None)
    };

    let call = target.call(&f.name, &call_args);
    let output = if throws(f) {
        quote! {
            __wv_out.map_err(|__wv_e| ::weaveffi::abi::FfiError::from_report(&__wv_e))
        }
    } else {
        quote!(::std::result::Result::Ok(__wv_out))
    };

    let object = user.ret_object();
    // Lowering can panic (an oversized buffer, say) and must not unwind past
    // the exactly-once promise, so it's caught and reported like a producer
    // panic. A `void` result has nothing to lower.
    let success_arm = match &f.ret {
        Some(ty) => {
            let (lower, slots) = async_result_slots(ty, object.as_ref(), user)?;
            quote! {
                ::std::result::Result::Ok(__wv_val) => {
                    match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(
                        move || {
                            #lower
                            (#(#slots,)*)
                        },
                    )) {
                        ::std::result::Result::Ok((#(#slots,)*)) => {
                            callback(__wv_ctx, ::std::ptr::null_mut() #(, #slots)*);
                            return;
                        }
                        ::std::result::Result::Err(__wv_panic) => {
                            ::weaveffi::abi::FfiError::from_panic(&*__wv_panic)
                        }
                    }
                }
            }
        }
        None => quote! {
            ::std::result::Result::Ok(()) => {
                callback(__wv_ctx, ::std::ptr::null_mut());
                return;
            }
        },
    };
    let attrs = thunk_attrs();

    Ok(quote! {
        #callback_typedef

        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) {
            #[allow(unused_unsafe)]
            unsafe {
                #pre_spawn
                let __wv_ctx = context as usize;
                ::weaveffi::abi::run_async(
                    async move {
                        #in_future
                        let __wv_out = #call.await;
                        #output
                    },
                    #cancel,
                    move |__wv_res: ::std::result::Result<_, ::weaveffi::abi::FfiError>| {
                        let __wv_ctx = __wv_ctx as *mut ::std::ffi::c_void;
                        let __wv_err = match __wv_res {
                            #success_arm
                            ::std::result::Result::Err(__wv_e) => __wv_e,
                        };
                        callback(
                            __wv_ctx,
                            ::weaveffi::abi::boxed_error(__wv_err)
                            #(, #sentinels)*
                        );
                    },
                );
            }
        }
    })
}
