//! Thunk emission for `iter<T>` functions: the launcher / `_next` /
//! `_destroy` trio.
//!
//! This is the producer half of the iterator pull contract (see
//! [`weaveffi_model::plan`]): each `_next` call yields exactly one element
//! the consumer then owns (written to the slots its
//! [`ItemPass`](weaveffi_model::plan::ItemPass) names), and `_destroy`
//! releases the handle exactly once. Errors from the launcher follow the
//! owning function's error strategy; `_next` fails only on a null handle or
//! out slot, or a concurrent or re-entrant `_next`.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{FnBinding, IteratorBinding};
use weaveffi_model::plan::ItemPass;

use super::helpers::{ctype_to_rust, fn_slots, ident, slot, thunk_attrs, CallTarget, UserSig};
use super::lift::{lower_item, slot_owners};
use super::sync::{checked_call, sync_lifts};
use super::unsupported;

/// Generate the launcher / `_next` / `_destroy` trio for a function returning
/// `iter<T>`. The producer returns a `weaveffi::Iter<T>` (optionally wrapped in
/// `Result`); the launcher boxes it behind a
/// [`weaveffi::abi::IterHandle`], `_next` pulls one element and lowers it
/// into its out slots, and `_destroy` drops the handle.
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
    let elem_object: Option<TokenStream> = match it.item {
        ItemPass::Object { .. } => user.iter_elem_object(),
        _ => None,
    };
    let attrs = thunk_attrs();

    // ── launcher: lift inputs, run the producer's fn, box the iterator ──
    let launch_sym = ident(&f.abi.symbol);
    let launch_params = fn_slots(&f.abi.params, &slot_owners(&f.params), user, prefix)?;
    let lifts = sync_lifts(f, user, target)?;
    let lifted = lifts.finish();
    let call = checked_call(f, target.call(&f.name, &lifts.args), user);
    let launch = quote! {
        #attrs
        pub unsafe extern "C" fn #launch_sym(#(#launch_params),*) -> *mut #handle {
            unsafe {
                ::weaveffi::abi::call_sync(__wv_out_err, move || unsafe {
                    #lifted
                    ::std::result::Result::Ok(::weaveffi::abi::iter_into_raw(#call))
                })
            }
        }
    };

    // ── next: pull one element, lower it into its out slots, return 1/0 ──
    let next_sym = ident(&it.next.symbol);
    // The first slot is the opaque handle (spelled with the real Rust type);
    // the item slots and `out_err` lower straight from the model, except an
    // object `out_item`, which uses the producer's pointee spelling.
    let object_slot = match &it.item {
        ItemPass::Object { out_item, .. } => Some(out_item.name.as_str()),
        _ => None,
    };
    let rest_params: Vec<TokenStream> = it.next.params[1..]
        .iter()
        .map(|p| {
            let n = slot(&p.name);
            match (&elem_object, object_slot) {
                (Some(obj), Some(o)) if o == p.name => quote!(#n: *mut *mut #obj),
                _ => {
                    let t = ctype_to_rust(&p.ty, prefix);
                    quote!(#n: #t)
                }
            }
        })
        .collect();
    let iter = slot(&it.next.params[0].name);
    let convert = match user.iter_elem_shape() {
        Some(shape) => {
            let lowered = shape.lower(quote!(&__wv_item));
            quote!(let __wv_item = #lowered;)
        }
        None => TokenStream::new(),
    };
    let (checks, write) = lower_item(&it.item, elem_object.as_ref());
    let next = quote! {
        #attrs
        pub unsafe extern "C" fn #next_sym(#iter: *const #handle, #(#rest_params),*) -> i32 {
            unsafe {
                ::weaveffi::abi::call_sync(__wv_out_err, move || unsafe {
                    #checks
                    match ::weaveffi::abi::iter_next(#iter)? {
                        ::std::option::Option::Some(__wv_item) => {
                            #convert
                            #write
                            ::std::result::Result::Ok(1)
                        }
                        ::std::option::Option::None => ::std::result::Result::Ok(0),
                    }
                })
            }
        }
    };

    // ── destroy: drop the handle exactly once. ──
    let destroy_sym = ident(&it.destroy_symbol);
    let destroy = quote! {
        #attrs
        pub unsafe extern "C" fn #destroy_sym(__wv_iter: *mut #handle) {
            unsafe { ::weaveffi::abi::iter_destroy(__wv_iter) }
        }
    };

    Ok(quote! {
        #launch
        #next
        #destroy
    })
}
