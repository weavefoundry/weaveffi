//! Thunk emission for `iter<T>` functions: the launcher / `_next` /
//! `_destroy` trio.
//!
//! This is the producer half of the pull contract stated by
//! [`weaveffi_model::plan::IteratorProtocol`]: each `_next` call yields exactly
//! one element the consumer then owns (and releases per the protocol's
//! `elem` pass), and `_destroy` releases the handle exactly once. Errors from
//! the launcher follow the owning function's
//! [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy); `_next` fails only
//! on a null handle or a concurrent or re-entrant `_next`.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{FnBinding, IteratorBinding};

use super::helpers::{fn_slots, ident, slot_tokens, thunk_attrs, CallTarget, UserSig};
use super::lift::lower_ret;
use super::sync::{checked_call, sync_lifts};
use super::unsupported;

/// Generate the launcher / `_next` / `_destroy` trio for a function returning
/// `iter<T>`. The producer returns a `weaveffi::Iter<T>` (optionally wrapped in
/// `Result`); the launcher boxes it behind a
/// [`weaveffi::abi::IterHandle`], `_next` pulls one element and lowers it
/// through `out_item`, and `_destroy` drops the handle.
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
    let lifts = sync_lifts(f, user, target)?;
    let lifted = lifts.finish();
    let call = checked_call(f, target.call(&f.name, &lifts.args), user);
    let launch = quote! {
        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) -> *mut #handle {
            unsafe {
                ::weaveffi::abi::call_sync(out_err, move || unsafe {
                    #lifted
                    ::std::result::Result::Ok(::weaveffi::abi::iter_into_raw(#call))
                })
            }
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
    let item = lower_ret(&it.elem, &quote!(__wv_item), elem_object.as_ref());
    let next = quote! {
        #attrs
        pub unsafe extern "C" fn #next_sym(iter: *const #handle, #(#rest_params),*) -> i32 {
            unsafe {
                ::weaveffi::abi::call_sync(out_err, move || unsafe {
                    if out_item.is_null() {
                        return ::std::result::Result::Err(::weaveffi::abi::FfiError::new(
                            ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            "out_item is null",
                        ));
                    }
                    ::std::result::Result::Ok(match ::weaveffi::abi::iter_next(iter)? {
                        ::std::option::Option::Some(__wv_item) => {
                            *out_item = #item;
                            1
                        }
                        ::std::option::Option::None => 0,
                    })
                })
            }
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
