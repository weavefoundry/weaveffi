//! Expansion of `weaveffi::export_runtime!()`: the fixed runtime surface
//! every library exports under its own prefix.
//!
//! The bodies live in `weaveffi-abi`; only the `extern "C"` thunks are
//! emitted here, in the producer's crate, because `#[no_mangle]` symbols in
//! a transitive `rlib` aren't guaranteed to be exported from a `cdylib`.
//!
//! `{prefix}_debug_live` is always exported. The macro can't see which cargo
//! features the producer enabled on its `weaveffi` dependency, and a
//! consumer shouldn't need to know either: without the `leak-check` feature
//! the function exists and reports `0` for every kind.

use proc_macro2::TokenStream;
use quote::quote;

use crate::codegen::{ident, prefix};

/// Expand `export_runtime!()` (which takes no arguments).
pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    if !input.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "weaveffi::export_runtime!() takes no arguments; the symbol prefix is the crate name",
        ));
    }
    let p = prefix()?;
    let sym = |name: &str| ident(&format!("{p}_{name}"));
    let abi_version = sym("abi_version");
    let error_set = sym("error_set");
    let error_clear = sym("error_clear");
    let error_free = sym("error_free");
    let free_bytes = sym("free_bytes");
    let token_create = sym("cancel_token_create");
    let token_cancel = sym("cancel_token_cancel");
    let token_is_cancelled = sym("cancel_token_is_cancelled");
    let token_destroy = sym("cancel_token_destroy");
    let debug_live = sym("debug_live");
    let alloc = sym("alloc");
    let dealloc = sym("dealloc");
    Ok(quote! {
            // Consumers compare this against the revision they were generated
            // for before touching any other symbol, so it must stay a plain
            // constant with no side effects.
            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #abi_version() -> u32 {
                ::weaveffi::abi::ABI_VERSION
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_set(
                err: *mut ::weaveffi::abi::FfiError,
                code: i32,
                message: *const ::std::os::raw::c_char,
            ) {
                unsafe { ::weaveffi::abi::error_set_c(err, code, message) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_clear(err: *mut ::weaveffi::abi::FfiError) {
                unsafe { ::weaveffi::abi::error_clear(err) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_free(err: *mut ::weaveffi::abi::FfiError) {
                unsafe { ::weaveffi::abi::error_free(err) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #free_bytes(ptr: *mut u8, len: usize) {
                unsafe { ::weaveffi::abi::free_bytes(ptr, len) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #token_create() -> *mut ::weaveffi::abi::FfiCancelToken {
                ::weaveffi::abi::cancel_token_create()
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #token_cancel(token: *mut ::weaveffi::abi::FfiCancelToken) {
                unsafe { ::weaveffi::abi::cancel_token_cancel(token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #token_is_cancelled(token: *const ::weaveffi::abi::FfiCancelToken) -> bool {
                unsafe { ::weaveffi::abi::cancel_token_is_cancelled(token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #token_destroy(token: *mut ::weaveffi::abi::FfiCancelToken) {
                unsafe { ::weaveffi::abi::cancel_token_destroy(token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #debug_live(kind: i32) -> u64 {
                ::weaveffi::abi::debug_live(kind)
            }

            // Wasm has no host allocator, so the generated JS glue stages
            // input buffers and return slots through these.
            #[cfg(target_arch = "wasm32")]
            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #alloc(size: u32) -> *mut u8 {
                ::weaveffi::abi::wasm_alloc(size as usize)
            }

            #[cfg(target_arch = "wasm32")]
            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #dealloc(ptr: *mut u8, size: u32) {
                unsafe { ::weaveffi::abi::wasm_dealloc(ptr, size as usize) }
            }
    })
}
