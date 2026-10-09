//! Expansion of `weaveffi::export_runtime!()`: the fixed runtime surface
//! every library exports under its own prefix.
//!
//! The bodies live in `weaveffi::abi`; only the `extern "C"` thunks are
//! emitted here, in the producer's crate, because `#[no_mangle]` symbols in
//! a transitive `rlib` aren't guaranteed to be exported from a `cdylib`.
//!
//! The expansion also defines the hidden module `crate::__weaveffi_runtime`,
//! which every `#[weaveffi::module]` references, so a crate that forgets to
//! call `export_runtime!()` (or calls it anywhere but the crate root) fails
//! to compile instead of producing a library with no runtime symbols.
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
    let error_set_payload = sym("error_set_payload");
    let error_clear = sym("error_clear");
    let error_free = sym("error_free");
    let free_bytes = sym("free_bytes");
    let token_create = sym("cancel_token_create");
    let token_cancel = sym("cancel_token_cancel");
    let token_is_cancelled = sym("cancel_token_is_cancelled");
    let token_destroy = sym("cancel_token_destroy");
    let debug_live = sym("debug_live");
    let alloc = sym("alloc");
    Ok(quote! {
            /// Proof that this crate exports the WeaveFFI runtime, which every
            /// `#[weaveffi::module]` checks for.
            #[doc(hidden)]
            pub mod __weaveffi_runtime {}

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
                __wv_err: *mut ::weaveffi::abi::FfiError,
                __wv_code: i32,
                __wv_message_ptr: *const u8,
                __wv_message_len: usize,
            ) {
                unsafe {
                    ::weaveffi::abi::error_set_c(
                        __wv_err,
                        __wv_code,
                        __wv_message_ptr,
                        __wv_message_len,
                    )
                }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_set_payload(
                __wv_err: *mut ::weaveffi::abi::FfiError,
                __wv_ptr: *const u8,
                __wv_len: usize,
            ) {
                unsafe { ::weaveffi::abi::error_set_payload_c(__wv_err, __wv_ptr, __wv_len) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_clear(__wv_err: *mut ::weaveffi::abi::FfiError) {
                unsafe { ::weaveffi::abi::error_clear(__wv_err) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #error_free(__wv_err: *mut ::weaveffi::abi::FfiError) {
                unsafe { ::weaveffi::abi::error_free(__wv_err) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #free_bytes(__wv_ptr: *mut u8, __wv_len: usize) {
                unsafe { ::weaveffi::abi::free_bytes(__wv_ptr, __wv_len) }
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
            pub unsafe extern "C" fn #token_cancel(__wv_token: *mut ::weaveffi::abi::FfiCancelToken) {
                unsafe { ::weaveffi::abi::cancel_token_cancel(__wv_token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #token_is_cancelled(
                __wv_token: *const ::weaveffi::abi::FfiCancelToken,
            ) -> bool {
                unsafe { ::weaveffi::abi::cancel_token_is_cancelled(__wv_token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn #token_destroy(__wv_token: *mut ::weaveffi::abi::FfiCancelToken) {
                unsafe { ::weaveffi::abi::cancel_token_destroy(__wv_token) }
            }

            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #debug_live(__wv_kind: i32) -> u64 {
                ::weaveffi::abi::debug_live(__wv_kind)
            }

            // Consumers allocate the runs they hand to the producer (a
            // callback's string, bytes, or buffer return) and, on wasm32,
            // stage arguments through this.
            #[doc(hidden)]
            #[unsafe(no_mangle)]
            #[allow(unsafe_code)]
            pub extern "C" fn #alloc(__wv_len: usize) -> *mut u8 {
                ::weaveffi::abi::alloc(__wv_len)
            }
    })
}
