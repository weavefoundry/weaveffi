//! Byte runs: the producer-allocated `(ptr, len)` memory every string,
//! bytes, value buffer, typed array, error message, and error payload
//! crosses in.
//!
//! These functions are the audited home of the pointer operations a WeaveFFI
//! producer performs on runs. The `#[weaveffi::module]` expansion wires the
//! generated thunks to them:
//!
//! * **lift** functions read a borrowed `(ptr, len)` parameter. They never
//!   take ownership of caller memory; the borrowing variants
//!   ([`lift_str`], [`lift_byte_slice`]) don't even copy.
//! * [`bytes_into_raw`] and [`slice_into_raw`] copy a value into a new run
//!   the consumer releases with `{prefix}_free_bytes` (see [`free_bytes`]).
//! * [`alloc`] is the body of `{prefix}_alloc`: a run the consumer fills and
//!   either frees with `{prefix}_free_bytes` or hands to the producer (a
//!   callback's string, bytes, buffer, or typed-array return), which adopts
//!   it with [`adopt_bytes`].
//!
//! Every run is allocated with **alignment 8** (`Layout::from_size_align(len,
//! 8)`) and released with the same layout, so a run can hold any element
//! type a typed array uses and both directions share one release function.
//! A run is always a fresh allocation: returning a `Vec` or `String` copies
//! its bytes out instead of handing over the vector's own storage, whose
//! alignment is only 1.
//!
//! Strings, bytes, and serialized value buffers all share this one
//! representation: a string is its UTF-8 bytes, never NUL-terminated, so an
//! interior NUL round-trips intact. A null pointer is valid only with a
//! length of `0` and denotes the empty run.

use std::alloc::Layout;

/// The alignment of every byte run, in bytes: enough for any element of a
/// typed array (`i64`, `u64`, `f64`).
pub const RUN_ALIGN: usize = 8;

/// The layout of a run of `len` bytes, or `None` when `len` is too large to
/// allocate.
fn run_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len, RUN_ALIGN).ok()
}

/// Borrow a `(ptr, len)` parameter as a byte slice for a caller-chosen
/// lifetime, without copying.
///
/// Returns `None` when `ptr` is null but `len` isn't `0`, which the thunk
/// reports as a marshalling failure.
///
/// # Safety
///
/// When `ptr` is non-null it must point to `len` initialized bytes that stay
/// valid and unmodified for the whole lifetime `'a`. Generated thunks bound
/// `'a` by the call, matching the contract that parameters are borrowed for
/// the call's duration.
#[must_use]
pub unsafe fn lift_byte_slice<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return (len == 0).then_some(&[][..]);
    }
    // SAFETY: the caller guarantees `ptr` covers `len` bytes valid for `'a`.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Copy a `(ptr, len)` parameter into an owned `Vec<u8>`.
///
/// Returns `None` when `ptr` is null but `len` isn't `0`.
///
/// # Safety
///
/// Same contract as [`lift_byte_slice`], for the duration of the call.
#[must_use]
pub unsafe fn lift_bytes(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_byte_slice(ptr, len) }.map(<[u8]>::to_vec)
}

/// Borrow a `(ptr, len)` string parameter as a `&str`, validating UTF-8,
/// without copying.
///
/// Returns `None` when `ptr` is null but `len` isn't `0`, or when the bytes
/// aren't valid UTF-8.
///
/// # Safety
///
/// Same contract as [`lift_byte_slice`].
#[must_use]
pub unsafe fn lift_str<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    // SAFETY: forwarded from the caller.
    std::str::from_utf8(unsafe { lift_byte_slice(ptr, len) }?).ok()
}

/// Copy a `(ptr, len)` string parameter into an owned `String`, validating
/// UTF-8.
///
/// Returns `None` when `ptr` is null but `len` isn't `0`, or when the bytes
/// aren't valid UTF-8.
///
/// # Safety
///
/// Same contract as [`lift_byte_slice`], for the duration of the call.
#[must_use]
pub unsafe fn lift_string(ptr: *const u8, len: usize) -> Option<String> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_str(ptr, len) }.map(str::to_owned)
}

/// Copy `data` into a new producer-allocated run, the `(ptr, len)` a return
/// slot, an async result, or an error message carries, to be released with
/// [`free_bytes`]. Empty data yields `(null, 0)` and allocates nothing.
#[must_use]
pub fn bytes_into_raw(data: &[u8]) -> (*const u8, usize) {
    if data.is_empty() {
        return (std::ptr::null(), 0);
    }
    let ptr = alloc_uninit(data.len());
    // SAFETY: `ptr` is a fresh allocation of `data.len()` bytes, which can't
    // overlap `data`.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
    (ptr.cast_const(), data.len())
}

/// Copy `items` into a new producer-allocated run holding a typed array
/// (8-aligned, so any element type is aligned), returning the pointer and
/// the **element count**. The consumer releases it with
/// `{prefix}_free_bytes((uint8_t*)ptr, count * sizeof(T))`. An empty slice
/// yields `(null, 0)`.
#[must_use]
pub fn slice_into_raw<T: Copy>(items: &[T]) -> (*mut T, usize) {
    const { assert!(std::mem::align_of::<T>() <= RUN_ALIGN) };
    let size = std::mem::size_of_val(items);
    if size == 0 {
        return (std::ptr::null_mut(), 0);
    }
    let ptr = alloc_uninit(size);
    // SAFETY: `ptr` is a fresh, 8-aligned allocation of `size` bytes, which
    // can't overlap `items`; `T: Copy` has no drop obligations.
    unsafe { std::ptr::copy_nonoverlapping(items.as_ptr().cast::<u8>(), ptr, size) };
    (ptr.cast::<T>(), items.len())
}

/// Lower an owned byte vector into a return value plus the trailing
/// `size_t* out_len` slot. See [`bytes_into_raw`].
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn lower_bytes(data: &[u8], out_len: *mut usize) -> *const u8 {
    let (ptr, len) = bytes_into_raw(data);
    if !out_len.is_null() {
        // SAFETY: the caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = len };
    }
    ptr
}

/// Lower a string as its UTF-8 bytes, exactly like [`lower_bytes`].
///
/// # Safety
///
/// Same contract as [`lower_bytes`].
pub unsafe fn lower_string(s: &str, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { lower_bytes(s.as_bytes(), out_len) }
}

/// Allocate an uninitialized run of `len > 0` bytes, counting it.
fn alloc_uninit(len: usize) -> *mut u8 {
    let layout = run_layout(len).unwrap_or_else(|| {
        // An object this large can't exist in memory, so neither can its
        // encoding; this is the allocator's own capacity-overflow failure.
        panic!("WeaveFFI run of {len} bytes exceeds the address space")
    });
    // SAFETY: `len > 0`, so the layout has a non-zero size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, 1);
    ptr
}

/// The body of `{prefix}_alloc`: allocate a zero-filled, 8-aligned run of
/// `len` bytes the consumer fills, released with [`free_bytes`] (the same
/// `len`) or handed to the producer, which adopts it. A `len` of `0`, or one
/// too large to allocate, returns null.
#[must_use]
pub fn alloc(len: usize) -> *mut u8 {
    let Some(layout) = run_layout(len).filter(|_| len > 0) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, 1);
    ptr
}

/// Adopt a run from [`alloc`] (or one this runtime returned) as an owned
/// vector, releasing the run. Null with a length of `0` is the empty run;
/// null with any other length yields `None`.
///
/// # Safety
///
/// `ptr` must be null or a run of exactly `len` bytes from [`alloc`] or
/// [`bytes_into_raw`] that nothing else releases or uses afterward.
#[must_use]
pub unsafe fn adopt_bytes(ptr: *mut u8, len: usize) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return (len == 0).then(Vec::new);
    }
    // SAFETY: the caller guarantees `ptr` covers `len` initialized bytes.
    let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    // SAFETY: the run is ours to release, exactly once.
    unsafe { free_bytes(ptr, len) };
    Some(out)
}

/// The body of `{prefix}_free_bytes`: release a run returned by this runtime
/// (a string, bytes, a value buffer, a typed array, or an error's message or
/// payload) or allocated with [`alloc`]. A null `ptr` or a `len` of `0` is a
/// no-op.
///
/// # Safety
///
/// `ptr` must be null or a pointer this runtime returned together with
/// `len` (for a typed array, `len` is its byte size), or [`alloc`] returned
/// for `len`, released exactly once.
pub unsafe fn free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Some(layout) = run_layout(len) else {
        // No run this large was ever allocated, so `ptr` isn't one of ours;
        // leaking is the only safe response to the contract violation.
        return;
    };
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, -1);
    // SAFETY: the caller hands back a run allocated with this layout,
    // exactly once.
    unsafe { std::alloc::dealloc(ptr, layout) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_roundtrip() {
        let mut len = 0usize;
        let ptr = unsafe { lower_bytes(&[1u8, 2, 3, 4], &mut len) };
        assert_eq!(len, 4);
        assert_eq!(unsafe { lift_bytes(ptr, len) }, Some(vec![1, 2, 3, 4]));
        unsafe { free_bytes(ptr.cast_mut(), len) };
    }

    #[test]
    fn every_run_is_8_aligned() {
        for len in [1usize, 3, 8, 13] {
            let (p, n) = bytes_into_raw(&vec![7u8; len]);
            assert_eq!(p as usize % RUN_ALIGN, 0);
            unsafe { free_bytes(p.cast_mut(), n) };
            let a = alloc(len);
            assert_eq!(a as usize % RUN_ALIGN, 0);
            unsafe { free_bytes(a, len) };
        }
        let (p, n) = slice_into_raw(&[1.5f64, -2.0]);
        assert_eq!((p as usize % RUN_ALIGN, n), (0, 2));
        assert_eq!(unsafe { std::slice::from_raw_parts(p, n) }, [1.5, -2.0]);
        unsafe { free_bytes(p.cast(), n * 8) };
    }

    #[test]
    fn empty_is_null_and_null_is_empty() {
        let mut len = 99usize;
        let ptr = unsafe { lower_bytes(&[], &mut len) };
        assert!(ptr.is_null());
        assert_eq!(len, 0);
        assert_eq!(unsafe { lift_str(std::ptr::null(), 0) }, Some(""));
        assert_eq!(unsafe { lift_bytes(std::ptr::null(), 0) }, Some(vec![]));
        assert_eq!(slice_into_raw::<u32>(&[]), (std::ptr::null_mut(), 0));
        unsafe { free_bytes(std::ptr::null_mut(), 0) };
    }

    #[test]
    fn null_with_a_length_is_rejected() {
        assert!(unsafe { lift_byte_slice(std::ptr::null(), 3) }.is_none());
        assert!(unsafe { lift_string(std::ptr::null(), 1) }.is_none());
    }

    #[test]
    fn strings_borrow_and_keep_interior_nul() {
        let text = "a\0b \u{1F980}";
        let s = unsafe { lift_str(text.as_ptr(), text.len()) }.unwrap();
        assert!(std::ptr::eq(s.as_ptr(), text.as_ptr()));
        assert_eq!(s, text);

        let mut len = 0usize;
        let ptr = unsafe { lower_string(text, &mut len) };
        assert_eq!(unsafe { lift_string(ptr, len) }.as_deref(), Some(text));
        unsafe { free_bytes(ptr.cast_mut(), len) };
    }

    #[test]
    fn alloc_runs_free_and_adopt() {
        assert!(alloc(0).is_null());
        assert!(alloc(usize::MAX).is_null());
        let p = alloc(4);
        assert_eq!(unsafe { std::slice::from_raw_parts(p, 4) }, [0; 4]);
        unsafe { free_bytes(p, 4) };
        let p = alloc(2);
        unsafe { *p = 9 };
        assert_eq!(unsafe { adopt_bytes(p, 2) }, Some(vec![9, 0]));
        assert_eq!(
            unsafe { adopt_bytes(std::ptr::null_mut(), 0) },
            Some(vec![])
        );
        assert_eq!(unsafe { adopt_bytes(std::ptr::null_mut(), 1) }, None);
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let bad = [0xFFu8, 0xFE];
        assert!(unsafe { lift_str(bad.as_ptr(), bad.len()) }.is_none());
    }
}
