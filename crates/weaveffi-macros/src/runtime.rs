//! Expansion of `weaveffi::export_runtime!()`: the fixed runtime surface
//! every library exports under its own prefix, and the crate's tag type.
//!
//! The bodies live in the `weaveffi` runtime; only the `extern "C"`
//! functions are emitted here, in the producer's crate, because exported
//! symbols in a transitive `rlib` aren't guaranteed to be exported from a
//! `cdylib`.
//!
//! The expansion also defines `crate::__weaveffi::Tag`, the type every
//! other attribute implements the runtime's value traits under, so a crate
//! that forgets to call `export_runtime!()` (or calls it anywhere but the
//! crate root) fails to compile instead of producing a library with no
//! runtime symbols.

use proc_macro2::TokenStream;
use quote::quote;

use crate::common::prefix;

/// Expand `export_runtime!()` (which takes no arguments).
pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    if !input.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "weaveffi::export_runtime!() takes no arguments; the symbol prefix is the crate name",
        ));
    }
    let p = prefix()?;
    let sym = |name: &str| format!("{p}_{name}");
    let abi_version = sym("abi_version");
    let error_set = sym("error_set");
    let error_set_payload = sym("error_set_payload");
    let error_clear = sym("error_clear");
    let error_free = sym("error_free");
    let alloc = sym("alloc");
    let run_free = sym("run_free");
    let token_create = sym("cancel_token_create");
    let token_cancel = sym("cancel_token_cancel");
    let token_is_cancelled = sym("cancel_token_is_cancelled");
    let token_destroy = sym("cancel_token_destroy");
    let debug_live = sym("debug_live");
    Ok(quote! {
        /// The crate's WeaveFFI tag: the type every exported item implements
        /// the runtime's value traits under.
        #[doc(hidden)]
        pub mod __weaveffi {
            /// The tag.
            #[derive(Debug)]
            pub struct Tag;
        }

        #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc)]
        const _: () = {
            // Consumers compare this against the revision they were
            // generated for before touching any other symbol.
            #[unsafe(export_name = #abi_version)]
            pub extern "C" fn __weaveffi_abi_version() -> u32 {
                ::weaveffi::__rt::ABI_VERSION
            }

            #[unsafe(export_name = #error_set)]
            pub unsafe extern "C" fn __weaveffi_error_set(
                err: *mut ::weaveffi::__rt::FfiError,
                code: i32,
                message_ptr: *const u8,
                message_len: usize,
            ) {
                unsafe { ::weaveffi::__rt::error_set_c(err, code, message_ptr, message_len) }
            }

            #[unsafe(export_name = #error_set_payload)]
            pub unsafe extern "C" fn __weaveffi_error_set_payload(
                err: *mut ::weaveffi::__rt::FfiError,
                ptr: *const u8,
                len: usize,
            ) {
                unsafe { ::weaveffi::__rt::error_set_payload_c(err, ptr, len) }
            }

            #[unsafe(export_name = #error_clear)]
            pub unsafe extern "C" fn __weaveffi_error_clear(err: *mut ::weaveffi::__rt::FfiError) {
                unsafe { ::weaveffi::__rt::error_clear(err) }
            }

            #[unsafe(export_name = #error_free)]
            pub unsafe extern "C" fn __weaveffi_error_free(err: *mut ::weaveffi::__rt::FfiError) {
                unsafe { ::weaveffi::__rt::error_free(err) }
            }

            // Consumers allocate the runs they hand to the producer (a
            // callback's string, bytes, buffer, or typed-array return) and,
            // on wasm32, stage arguments through this.
            #[unsafe(export_name = #alloc)]
            pub extern "C" fn __weaveffi_alloc(len: usize) -> *mut u8 {
                ::weaveffi::__rt::alloc(len)
            }

            #[unsafe(export_name = #run_free)]
            pub unsafe extern "C" fn __weaveffi_run_free(run: ::weaveffi::__rt::Run) {
                unsafe { ::weaveffi::__rt::run_free(run) }
            }

            #[unsafe(export_name = #token_create)]
            pub extern "C" fn __weaveffi_cancel_token_create() -> *mut ::weaveffi::__rt::FfiCancelToken {
                ::weaveffi::__rt::cancel_token_create()
            }

            #[unsafe(export_name = #token_cancel)]
            pub unsafe extern "C" fn __weaveffi_cancel_token_cancel(
                token: *mut ::weaveffi::__rt::FfiCancelToken,
            ) {
                unsafe { ::weaveffi::__rt::cancel_token_cancel(token) }
            }

            #[unsafe(export_name = #token_is_cancelled)]
            pub unsafe extern "C" fn __weaveffi_cancel_token_is_cancelled(
                token: *const ::weaveffi::__rt::FfiCancelToken,
            ) -> bool {
                unsafe { ::weaveffi::__rt::cancel_token_is_cancelled(token) }
            }

            #[unsafe(export_name = #token_destroy)]
            pub unsafe extern "C" fn __weaveffi_cancel_token_destroy(
                token: *mut ::weaveffi::__rt::FfiCancelToken,
            ) {
                unsafe { ::weaveffi::__rt::cancel_token_destroy(token) }
            }

            // Always exported: without the `leak-check` feature every kind
            // but `-1` reads `0`, so a consumer never has to know how the
            // producer was built.
            #[unsafe(export_name = #debug_live)]
            pub extern "C" fn __weaveffi_debug_live(kind: i32) -> u64 {
                ::weaveffi::__rt::debug_live(kind)
            }
        };
    })
}
