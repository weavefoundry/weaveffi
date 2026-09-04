//! Thunk emission for `iter<T>` functions: the launcher / `_next` /
//! `_destroy` trio.
//!
//! This is the producer half of the pull contract stated by
//! [`weaveffi_core::plan::IteratorProtocol`]: each `_next` call yields exactly
//! one element the consumer then owns (and releases per the protocol's
//! `elem` pass), and `_destroy` releases the handle exactly once. Errors from
//! the launcher and from each `_next` follow the owning function's
//! [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy).

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::{FnBinding, IteratorBinding};

use super::helpers::{fn_slots, ident, slot_tokens, wrap_unwind, CallTarget, UserSig};
use super::marshal::{lift_param, lower_value};
use super::sync::throws;
use super::unsupported;

/// Generate the launcher / `_next` / `_destroy` trio for a function returning
/// `iter<T>`. The producer returns a `weaveffi::Iter<T>` (optionally wrapped in
/// `Result`); the launcher boxes it behind the opaque iterator handle, `_next`
/// pulls one element and lowers it through `out_item`, and `_destroy` drops the
/// box.
pub(crate) fn gen_iterator_function(
    f: &FnBinding,
    it: &IteratorBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
) -> syn::Result<TokenStream> {
    // The handle type is the producer's own `Iter<X>` spelling (with `Result`
    // peeled), so the element type, including its map flavor and any
    // `super::` path, is exactly what the producer's iterator yields.
    let iter_rust = user
        .ret_type()
        .ok_or_else(|| unsupported(&f.name, "iterator return without a source type"))?;
    // The object pointee for an element that is an interface (`Arc<T>` or
    // `Option<Arc<T>>`), spelled the producer's way.
    let elem_object: Option<TokenStream> = if it.elem.interface_name().is_some() {
        user.iter_elem_object()
    } else {
        None
    };

    // ── launcher: lift inputs, run the user fn, box the iterator ──
    let launch_sym = ident(&it.launch.symbol);
    let launch_params = fn_slots(&it.launch.params, &f.params, user)?;
    let launch_sentinel = quote!(::std::ptr::null_mut());

    let self_pre = target.self_preamble(&launch_sentinel, user.receiver_is_arc());
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let (pre, arg) = lift_param(pb, user, &launch_sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }
    let call = target.call(&f.name, &call_args);
    let bind_iter = if throws(f) {
        quote! {
            let __wv_iter = match #call {
                ::std::result::Result::Ok(__v) => __v,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set_with_payload(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                        ::weaveffi::abi::ErrorReport::payload(&__wv_err),
                    );
                    return ::std::ptr::null_mut();
                }
            };
        }
    } else {
        quote!(let __wv_iter = #call;)
    };
    let launch_body = wrap_unwind(
        quote! {
            #self_pre
            #preamble
            #bind_iter
            ::weaveffi::abi::error_set_ok(out_err);
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_iter))
        },
        Some(&launch_sentinel),
    );
    let launch = quote! {
        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #launch_sym(#(#launch_params),*) -> *mut #iter_rust {
            #launch_body
        }
    };

    // ── next: pull one element, lower it into `out_item`, return 1/0 ──
    let next_sym = ident(&it.next.symbol);
    // The first slot is the opaque `iter` handle (spelled with the real Rust
    // type); the rest (`out_item`, any item out-params, `out_err`) lower
    // straight from the model, except an object `out_item`, which uses the
    // producer's pointee spelling.
    let rest_params: Vec<TokenStream> = it.next.params[1..]
        .iter()
        .map(|p| match (&elem_object, p.name.as_str()) {
            (Some(obj), "out_item") => quote!(out_item: *mut *mut #obj),
            _ => slot_tokens(p),
        })
        .collect();
    let item_lowered = lower_value(&it.elem, quote!(__wv_item), elem_object.as_ref())?;
    let next_sentinel = quote!(0);
    let next_body = wrap_unwind(
        quote! {
            if iter.is_null() || out_item.is_null() {
                ::weaveffi::abi::error_set(
                    out_err,
                    ::weaveffi::abi::MARSHAL_ERROR_CODE,
                    "iterator or out_item is null",
                );
                return 0;
            }
            let __wv_it = unsafe { &mut *iter };
            match ::std::iter::Iterator::next(__wv_it) {
                ::std::option::Option::Some(__wv_item) => {
                    ::weaveffi::abi::error_set_ok(out_err);
                    let __wv_slot = #item_lowered;
                    unsafe { *out_item = __wv_slot };
                    1
                }
                ::std::option::Option::None => {
                    ::weaveffi::abi::error_set_ok(out_err);
                    0
                }
            }
        },
        Some(&next_sentinel),
    );
    let next = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #next_sym(iter: *mut #iter_rust, #(#rest_params),*) -> i32 {
            #next_body
        }
    };

    // ── destroy: drop the box; a panicking user `Drop` is swallowed (there is
    // no `out_err` slot to report through, and a destructor must not abort),
    // and so is a foreign failure a `Drop` deferred by calling back into the
    // consumer, which must not leak into the next unrelated thunk. The
    // consumer calls this exactly once, per the iterator protocol's
    // handle-lifecycle clause. ──
    let destroy_sym = ident(&it.destroy_symbol);
    let destroy = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(iter: *mut #iter_rust) {
            if !iter.is_null() {
                let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    unsafe { drop(::std::boxed::Box::from_raw(iter)) }
                }));
                let _ = ::weaveffi::abi::take_foreign_error();
            }
        }
    };

    Ok(quote! {
        #launch
        #next
        #destroy
    })
}
