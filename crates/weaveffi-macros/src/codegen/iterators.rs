//! Thunk emission for `iter<T>` functions: the launcher / `_next` /
//! `_destroy` trio.
//!
//! This is the producer half of the pull contract stated by
//! [`weaveffi_model::plan::IteratorProtocol`]: each `_next` call yields exactly
//! one element the consumer then owns (and releases per the protocol's
//! `elem` pass), and `_destroy` releases the handle exactly once. Errors from
//! the launcher and from each `_next` follow the owning function's
//! [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy).

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{FnBinding, IteratorBinding};

use super::helpers::{
    deferred_failure_check, fn_slots, ident, reject, slot_tokens, thunk_attrs, wrap_unwind,
    CallTarget, UserSig,
};
use super::marshal::{lift_param, lower_value};
use super::sync::throws;
use super::unsupported;

/// Generate the launcher / `_next` / `_destroy` trio for a function returning
/// `iter<T>`. The producer returns a `weaveffi::Iter<T>` (optionally wrapped in
/// `Result`); the launcher boxes it behind a
/// [`weaveffi_abi::IterHandle`], `_next` pulls one element under the
/// handle's lock and lowers it through `out_item`, and `_destroy` drops the
/// handle.
pub(crate) fn gen_iterator_function(
    f: &FnBinding,
    it: &IteratorBinding,
    user: &UserSig<'_>,
    target: &CallTarget,
    prefix: &str,
) -> syn::Result<TokenStream> {
    // The element type is the producer's own `Iter<X>` spelling, so its map
    // flavor and any `super::` path are exactly what the iterator yields.
    let elem = user.iter_elem_type().ok_or_else(|| {
        unsupported(
            user.ret_span(),
            &f.name,
            "iterator return without an element type",
        )
    })?;
    let handle = quote!(::weaveffi::abi::IterHandle<#elem>);
    // The object pointee for an element that is an interface (`Arc<T>` or
    // `Option<Arc<T>>`), spelled the producer's way.
    let elem_object: Option<TokenStream> = if it.elem.interface_name().is_some() {
        user.iter_elem_object()
    } else {
        None
    };
    let attrs = thunk_attrs();

    // ── launcher: lift inputs, run the producer's fn, box the iterator ──
    let launch_sym = ident(&it.launch.symbol);
    let launch_params = fn_slots(&it.launch.params, &f.params, user, prefix)?;
    let null = quote!(::std::ptr::null_mut());
    let launch_sentinel = Some(&null);

    let self_pre = target.self_preamble(launch_sentinel, user.receiver_is_arc());
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let (pre, arg) = lift_param(pb, user, launch_sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }
    let call = target.call(&f.name, &call_args);
    let deferred = deferred_failure_check(launch_sentinel);
    let bind_iter = if throws(f) {
        quote! {
            let __wv_iter = match __wv_out {
                ::std::result::Result::Ok(__wv_v) => __wv_v,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_store(
                        out_err,
                        ::weaveffi::abi::FfiError::from_report(&__wv_err),
                    );
                    return ::std::ptr::null_mut();
                }
            };
        }
    } else {
        quote!(let __wv_iter = __wv_out;)
    };
    let launch_body = wrap_unwind(
        quote! {
            #self_pre
            #preamble
            let __wv_out = #call;
            #deferred
            #bind_iter
            ::weaveffi::abi::error_clear(out_err);
            ::weaveffi::abi::iter_into_raw(__wv_iter)
        },
        launch_sentinel,
    );
    let launch = quote! {
        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) -> *mut #handle {
            #launch_body
        }
    };

    // ── next: pull one element, lower it into `out_item`, return 1/0 ──
    let next_sym = ident(&it.next.symbol);
    // The first slot is the opaque handle (spelled with the real Rust type);
    // the rest (`out_item`, any item out-params, `out_err`) lower straight
    // from the model, except an object `out_item`, which uses the producer's
    // pointee spelling.
    let rest_params: Vec<TokenStream> = it.next.params[1..]
        .iter()
        .map(|p| match (&elem_object, p.name.as_str()) {
            (Some(obj), "out_item") => quote!(out_item: *mut *mut #obj),
            _ => slot_tokens(p, prefix),
        })
        .collect();
    let item_lowered = lower_value(&it.elem, quote!(__wv_item), elem_object.as_ref(), user)?;
    let zero = quote!(0);
    let next_sentinel = Some(&zero);
    let null_fail = reject("iterator or out_item is null", next_sentinel);
    let deferred = deferred_failure_check(next_sentinel);
    let next_body = wrap_unwind(
        quote! {
            if out_item.is_null() {
                #null_fail
            }
            let __wv_pulled = match ::weaveffi::abi::iter_next(iter) {
                ::std::option::Option::Some(__wv_p) => __wv_p,
                ::std::option::Option::None => { #null_fail }
            };
            #deferred
            ::weaveffi::abi::error_clear(out_err);
            match __wv_pulled {
                ::std::option::Option::Some(__wv_item) => {
                    *out_item = #item_lowered;
                    1
                }
                ::std::option::Option::None => 0,
            }
        },
        next_sentinel,
    );
    let next = quote! {
        #attrs
        pub unsafe extern "C" fn #next_sym(iter: *const #handle, #(#rest_params),*) -> i32 {
            #next_body
        }
    };

    // ── destroy: drop the handle exactly once, per the iterator protocol's
    // handle-lifecycle clause. ──
    let destroy_sym = ident(&it.destroy_symbol);
    let destroy = quote! {
        #attrs
        pub unsafe extern "C" fn #destroy_sym(iter: *mut #handle) {
            unsafe { ::weaveffi::abi::iter_destroy(iter) }
        }
    };

    Ok(quote! {
        #launch
        #next
        #destroy
    })
}
