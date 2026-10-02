//! C ABI runtime: the error struct, memory helpers, reference-counted
//! objects, cancel tokens, callback-interface vtables, iterators, the
//! value-buffer codec, and the async executor hook.
//!
//! Producers don't use this crate directly. The `#[weaveffi::module]`
//! expansion and `weaveffi::export_runtime!()` call into it (re-exported as
//! `weaveffi::abi`), so every `unsafe` pointer operation a producer performs
//! has one audited home. The normative description of the contract it
//! implements is `docs/src/reference/abi.md` in the WeaveFFI repository.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::missing_safety_doc)]
#![warn(clippy::doc_markdown)]
#![allow(unsafe_code)]

pub mod buffer;
pub mod callback;
pub mod cancel;
pub mod convert;
pub mod error;
pub mod iter;
pub mod leak;
pub mod object;
pub mod spawn;

pub use buffer::{
    decode_value, encode_value, BufferDecodeError, BufferReader, BufferValue, BufferWriter,
    ByValue, FixedWidth,
};
pub use callback::{
    defer_foreign_error, foreign_status, lift_callback, raise_foreign_error, take_foreign_error,
    CallbackInterface, ForeignCallback, ForeignError, ThunkScope, Vtable,
};
pub use cancel::{
    cancel_token_cancel, cancel_token_create, cancel_token_destroy, cancel_token_is_cancelled,
    CancelToken, Cancellable, FfiCancelToken,
};
pub use convert::{
    bytes_into_raw, free_bytes, lift_byte_slice, lift_bytes, lift_str, lift_string, lower_bytes,
    lower_string,
};
pub use error::{
    boxed_error, error_clear, error_free, error_set, error_set_c, error_store, panic_message,
    ErrorReport, FfiError, CANCELLED_ERROR_CODE, FOREIGN_ERROR_CODE, GENERIC_ERROR_CODE,
    MARSHAL_ERROR_CODE, PANIC_ERROR_CODE,
};
pub use iter::{iter_destroy, iter_into_raw, iter_next, Iter, IterHandle};
pub use leak::debug_live;
pub use object::{
    lower_object, lower_object_opt, object_arc, object_clone, object_destroy, object_from_token,
    object_ref, object_to_token,
};
pub use spawn::{
    block_on, run_async, set_spawner, spawn, BoxFuture, CatchUnwind, Spawner, SpawnerAlreadySet,
};

/// The revision of the WeaveFFI C ABI this runtime implements.
///
/// Every producer exports it as `{prefix}_abi_version()` (via
/// `weaveffi::export_runtime!()`), and every generated consumer compares it
/// with the revision it was generated against when it loads the library,
/// turning a silent memory-layout mismatch into a clear error.
///
/// The number only changes when the runtime surface (the error layout, the
/// value-buffer encoding, the object or callback-interface conventions, or
/// the set and signatures of the runtime symbols) changes incompatibly. It's
/// independent of the crate version and of the IDL schema version.
///
/// Revision 3 prefixed every runtime symbol with the library's own C prefix,
/// passed strings as `(ptr, len)` UTF-8 runs freed with `{prefix}_free_bytes`
/// (removing `free_string`), made cancel tokens reference counted, and added
/// [`CANCELLED_ERROR_CODE`] and the per-module contract checksums.
pub const ABI_VERSION: u32 = 3;

/// Fixed alignment used for every Wasm linear-memory allocation handed to JS.
///
/// 8 bytes over-aligns scalar and byte buffers but is required for the
/// `{i32 ptr, i32 len}` and wider return slots that JS reads back through
/// `DataView`.
#[cfg(target_arch = "wasm32")]
const WASM_ALLOC_ALIGN: usize = 8;

/// The body of `{prefix}_alloc` (`wasm32` only): allocate `size` bytes in this
/// module's linear memory.
///
/// The Wasm backend has no host-provided allocator, so the generated JS glue
/// stages input strings and buffers, and reserves return slots, through this.
/// The caller releases the block with [`wasm_dealloc`] using the same `size`.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn wasm_alloc(size: usize) -> *mut u8 {
    let size = size.max(1);
    match std::alloc::Layout::from_size_align(size, WASM_ALLOC_ALIGN) {
        // SAFETY: the layout has a non-zero size.
        Ok(layout) => unsafe { std::alloc::alloc(layout) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// The body of `{prefix}_dealloc` (`wasm32` only): release a block from
/// [`wasm_alloc`].
///
/// # Safety
///
/// `ptr` must be null or a block [`wasm_alloc`] returned for this same
/// `size`, released exactly once.
#[cfg(target_arch = "wasm32")]
pub unsafe fn wasm_dealloc(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(size.max(1), WASM_ALLOC_ALIGN) {
        // SAFETY: `ptr` came from `wasm_alloc` with this exact layout.
        unsafe { std::alloc::dealloc(ptr, layout) };
    }
}
