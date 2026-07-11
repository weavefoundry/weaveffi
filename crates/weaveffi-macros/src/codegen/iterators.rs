//! Thunk emission for `iter<T>` functions: the launcher / `_next` /
//! `_destroy` trio.
//!
//! This is the producer half of the pull contract stated by
//! [`weaveffi_core::plan::IteratorProtocol`]: each `_next` call yields exactly
//! one element the consumer then owns (and frees per the protocol's
//! `elem_free`), and `_destroy` releases the handle exactly once. Errors from
//! the launcher and from each `_next` follow the owning function's
//! [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy).

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::{FnBinding, IteratorBinding};
use weaveffi_ir::ir::TypeRef;

use super::helpers::{
    fn_slots, ident, param_is_ref, slot_tokens, typeref_to_rust, wrap_unwind, CallTarget,
};
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
    sig: &syn::Signature,
    target: &CallTarget,
) -> syn::Result<TokenStream> {
    let elem_rust = typeref_to_rust(&it.elem)?;
    let iter_rust = quote!(::weaveffi::Iter<#elem_rust>);

    // ── launcher: lift inputs, run the user fn, box the iterator ──
    let launch_sym = ident(&it.launch.symbol);
    let launch_params: Vec<TokenStream> = fn_slots(&it.launch.params, f, sig);
    let launch_sentinel = quote!(::std::ptr::null_mut());

    let self_pre = target.self_preamble(&launch_sentinel);
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let is_ref = param_is_ref(sig, &pb.name);
        let (pre, arg) = lift_param(pb, is_ref, &launch_sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }
    let call = target.call(&f.name, &call_args);
    let bind_iter = if throws(f) {
        quote! {
            let __wv_iter = match #call {
                ::std::result::Result::Ok(__v) => __v,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
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
    // straight from the model.
    let rest_params: Vec<TokenStream> = it.next.params[1..].iter().map(slot_tokens).collect();
    let item_lowered = lower_value(&it.elem, quote!(__wv_item))?;
    if matches!(it.elem, TypeRef::Map(_, _)) {
        return Err(unsupported("iterator", "map element type"));
    }
    let next_sentinel = quote!(0);
    let next_body = wrap_unwind(
        quote! {
            if iter.is_null() || out_item.is_null() {
                ::weaveffi::abi::error_set(out_err, -1, "iterator or out_item is null");
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
    // no `out_err` slot to report through, and a destructor must not abort).
    // The consumer calls this exactly once, per the iterator protocol's
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
            }
        }
    };

    Ok(quote! {
        #launch
        #next
        #destroy
    })
}
