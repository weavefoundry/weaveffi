//! Marshalling helpers that bridge owned Rust values and the C ABI slots.
//!
//! These functions are the audited home of every `unsafe` pointer operation a
//! WeaveFFI producer performs. The [`weaveffi-macros`](https://docs.rs/weaveffi-macros)
//! `#[weaveffi::module]` expansion wires the generated `extern "C"` thunks to
//! these helpers; producers never write the marshalling by hand. Keeping the
//! conversions in one place (rather than re-deriving them per generated symbol)
//! is what lets the runtime guarantee memory ownership rules consistently:
//!
//! * **lift** functions (`c_* -> Rust`) borrow or copy a foreign-supplied slot
//!   into a Rust value for the duration of a call; they never take ownership
//!   of caller memory.
//! * **lower** functions (`Rust -> c_*`) hand an owned, heap-allocated value to
//!   the foreign caller, who later releases it through the matching
//!   `weaveffi_free_*` / `*_destroy` entry point.
//!
//! Strings cross as NUL-terminated pointers via
//! [`string_to_c_ptr`](crate::string_to_c_ptr) (freed with
//! `weaveffi_free_string`); bytes and serialized value buffers cross as a
//! `(ptr, len)` pair via [`lower_bytes`] (freed with `weaveffi_free_bytes`);
//! everything composite crosses inside a value buffer (see [`crate::buffer`]),
//! so there are no per-shape array helpers here.

// ── Lifting: C ABI slot -> Rust value ─────────────────────────────────────

/// Copy a foreign byte buffer (`ptr` + `len`) into an owned `Vec<u8>`.
///
/// A null `ptr` (or `len == 0`) yields an empty vector. The returned vector
/// owns its bytes; the caller's buffer is left untouched.
///
/// # Safety
///
/// When `ptr` is non-null it must point to at least `len` initialized bytes
/// that stay valid for the duration of the call.
#[must_use]
pub unsafe fn lift_bytes(ptr: *const u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: caller guarantees `ptr` covers `len` initialized bytes.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Borrow a foreign byte buffer (`ptr` + `len`) as a `&[u8]` slice for the
/// lifetime `'a` the caller chooses.
///
/// A null `ptr` (or `len == 0`) yields an empty slice. No copy is made, so this
/// is the marshalling for a borrowed `&[u8]` parameter.
///
/// # Safety
///
/// When `ptr` is non-null it must point to at least `len` initialized bytes
/// that remain valid and immutable for the entire chosen lifetime `'a`.
#[must_use]
pub unsafe fn lift_byte_slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: caller guarantees `ptr` covers `len` bytes valid for `'a`.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

// ── Lowering: owned Rust value -> C ABI slot ─────────────────────────────

/// Lower an owned byte buffer into a heap allocation the caller frees with
/// `weaveffi_free_bytes`, writing the byte count through `out_len`.
///
/// An empty buffer yields a null pointer and a length of `0`. The allocation is
/// a boxed slice, matching the layout [`free_bytes`](crate::free_bytes)
/// reconstructs.
///
/// # Safety
///
/// `out_len`, when non-null, must point to a writable `usize`.
pub unsafe fn lower_bytes(data: Vec<u8>, out_len: *mut usize) -> *const u8 {
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = data.len() };
    }
    if data.is_empty() {
        return std::ptr::null();
    }
    let boxed = data.into_boxed_slice();
    Box::into_raw(boxed) as *const u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::free_bytes;

    #[test]
    fn bytes_roundtrip() {
        let data = vec![1u8, 2, 3, 4];
        let mut len = 0usize;
        let ptr = unsafe { lower_bytes(data.clone(), &mut len) } as *mut u8;
        assert_eq!(len, 4);
        let back = unsafe { lift_bytes(ptr, len) };
        assert_eq!(back, data);
        free_bytes(ptr, len);
    }

    #[test]
    fn empty_bytes_is_null() {
        let mut len = 99usize;
        let ptr = unsafe { lower_bytes(Vec::new(), &mut len) };
        assert!(ptr.is_null());
        assert_eq!(len, 0);
    }

    #[test]
    fn lift_byte_slice_is_borrow() {
        let data = [9u8, 8, 7];
        let s = unsafe { lift_byte_slice(data.as_ptr(), data.len()) };
        assert_eq!(s, &data);
        let empty = unsafe { lift_byte_slice::<'static>(std::ptr::null(), 0) };
        assert!(empty.is_empty());
    }
}
