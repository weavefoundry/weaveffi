//! Macros that user cdylibs invoke to expose the fixed WeaveFFI C ABI
//! runtime surface from a single line of Rust.
//!
//! Every Rust cdylib that hosts generated WeaveFFI bindings must expose
//! a small set of `#[no_mangle] extern "C"` functions that the language
//! wrappers call into: string/byte deallocation, error clearing, and
//! the cancel-token lifecycle. The wrappers themselves live in
//! `weaveffi-abi` as ordinary `pub fn`s; the `extern "C"` thunks must
//! be emitted in the *consumer's* crate because `#[no_mangle]` symbols
//! in a transitive `rlib` are not guaranteed to be re-exported from a
//! cdylib.
//!
//! Use [`export_runtime!`] once in your cdylib's `lib.rs` to wire all
//! of them up.

/// Emit `#[no_mangle] extern "C"` thunks for every runtime symbol that
/// the WeaveFFI generators expect to find in the consuming cdylib.
///
/// The macro expands to a fixed set of functions named with the
/// `weaveffi_` prefix:
///
/// - `weaveffi_free_string`
/// - `weaveffi_free_bytes`
/// - `weaveffi_error_clear`
/// - `weaveffi_cancel_token_create`
/// - `weaveffi_cancel_token_cancel`
/// - `weaveffi_cancel_token_is_cancelled`
/// - `weaveffi_cancel_token_destroy`
/// - `weaveffi_arena_create`
/// - `weaveffi_arena_register`
/// - `weaveffi_arena_destroy`
///
/// On `wasm32` targets it additionally emits a linear-memory allocator that the
/// generated JS bindings call to stage inputs and return slots:
///
/// - `weaveffi_alloc`
/// - `weaveffi_dealloc`
///
/// # Example
///
/// ```ignore
/// // Inside a cdylib's src/lib.rs
/// use weaveffi_abi as abi;
///
/// abi::export_runtime!();
///
/// // ... your generated/hand-written #[no_mangle] business functions ...
/// ```
///
/// Invoke this macro at module scope **exactly once** in your cdylib.
/// Multiple invocations would produce duplicate symbol definitions.
#[macro_export]
macro_rules! export_runtime {
    () => {
        #[no_mangle]
        pub extern "C" fn weaveffi_free_string(ptr: *const ::std::os::raw::c_char) {
            $crate::free_string(ptr)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
            $crate::free_bytes(ptr, len)
        }

        // WASM has no host allocator, so the generated JS glue stages input
        // buffers and `sret` slots through these thunks. They are only emitted
        // for `wasm32` targets; native targets allocate on the foreign side and
        // never reference them, keeping their ABI surface unchanged.
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn weaveffi_alloc(size: u32) -> *mut u8 {
            $crate::wasm_alloc(size as usize)
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn weaveffi_dealloc(ptr: *mut u8, size: u32) {
            $crate::wasm_dealloc(ptr, size as usize)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_error_clear(err: *mut $crate::weaveffi_error) {
            $crate::error_clear(err)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_cancel_token_create() -> *mut $crate::weaveffi_cancel_token {
            $crate::cancel_token_create()
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_cancel_token_cancel(token: *mut $crate::weaveffi_cancel_token) {
            $crate::cancel_token_cancel(token)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_cancel_token_is_cancelled(
            token: *const $crate::weaveffi_cancel_token,
        ) -> bool {
            $crate::cancel_token_is_cancelled(token)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_cancel_token_destroy(token: *mut $crate::weaveffi_cancel_token) {
            $crate::cancel_token_destroy(token)
        }

        // The batch-free arena is part of the documented stable runtime ABI, so
        // its thunks are re-exported here for the same reason as the rest: an
        // rlib-local `#[no_mangle]` is not guaranteed to survive into the cdylib.
        #[no_mangle]
        pub extern "C" fn weaveffi_arena_create() -> *mut $crate::arena::HandleArena {
            $crate::arena::arena_create()
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_arena_register(
            arena: *mut $crate::arena::HandleArena,
            ptr: *mut ::std::ffi::c_void,
            dtor: unsafe extern "C" fn(*mut ::std::ffi::c_void),
        ) {
            $crate::arena::arena_register(arena, ptr, dtor)
        }

        #[no_mangle]
        pub extern "C" fn weaveffi_arena_destroy(arena: *mut $crate::arena::HandleArena) {
            $crate::arena::arena_destroy(arena)
        }
    };
}
