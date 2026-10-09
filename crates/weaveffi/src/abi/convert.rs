//! Marshalling helpers that bridge Rust values and the C ABI's `(ptr, len)`
//! slots.
//!
//! These functions are the audited home of the pointer operations a WeaveFFI
//! producer performs on strings, bytes, and value buffers. The
//! `#[weaveffi::module]` expansion wires the generated thunks to them:
//!
//! * **lift** functions read a borrowed `(ptr, len)` parameter. They never
//!   take ownership of caller memory; the borrowing variants
//!   ([`lift_str`], [`lift_byte_slice`]) don't even copy.
//! * **lower** functions hand an owned value to the consumer as a
//!   producer-allocated `(ptr, len)` run, which the consumer releases with
//!   `{prefix}_free_bytes` (see [`free_bytes`]).
//! * [`alloc`] is the body of `{prefix}_alloc`: a run the consumer fills and
//!   either frees with `{prefix}_free_bytes` or hands to the producer (a
//!   callback's string, bytes, or buffer return), which adopts it with
//!   [`adopt_bytes`]. Every run is a `Box<[u8]>` allocation, so one release
//!   function covers both directions.
//!
//! Strings, bytes, and serialized value buffers all share this one
//! representation: a string is its UTF-8 bytes, never NUL-terminated, so an
//! interior NUL round-trips intact. A null pointer is valid only with a
//! length of `0` and denotes the empty run.

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

/// Turn an owned byte vector into the producer-allocated `(ptr, len)` run a
/// return slot or async result carries, to be released with
/// [`free_bytes`]. An empty vector yields `(null, 0)` and allocates nothing.
///
/// When the vector's capacity equals its length (as it does for every
/// encoding [`encode_value`](crate::abi::encode_value) produces) the conversion
/// reuses the allocation as is; otherwise it's shrunk to fit first.
#[must_use]
pub fn bytes_into_raw(data: Vec<u8>) -> (*const u8, usize) {
    if data.is_empty() {
        return (std::ptr::null(), 0);
    }
    let len = data.len();
    let ptr = Box::into_raw(data.into_boxed_slice())
        .cast::<u8>()
        .cast_const();
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, 1);
    (ptr, len)
}

/// Lower an owned byte vector into a return value plus the trailing
/// `size_t* out_len` slot. See [`bytes_into_raw`].
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn lower_bytes(data: Vec<u8>, out_len: *mut usize) -> *const u8 {
    let (ptr, len) = bytes_into_raw(data);
    if !out_len.is_null() {
        // SAFETY: the caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = len };
    }
    ptr
}

/// Lower an owned string as its UTF-8 bytes, exactly like [`lower_bytes`].
///
/// # Safety
///
/// Same contract as [`lower_bytes`].
pub unsafe fn lower_string(s: String, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { lower_bytes(s.into_bytes(), out_len) }
}

/// The body of `{prefix}_alloc`: allocate a zero-filled run of `len` bytes
/// the consumer fills, released with [`free_bytes`] (the same `len`) or
/// handed to the producer, which adopts it. A `len` of `0` returns null,
/// which is the empty run.
#[must_use]
pub fn alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, 1);
    Box::into_raw(vec![0u8; len].into_boxed_slice()).cast::<u8>()
}

/// Adopt a run from [`alloc`] (or one this runtime returned) as an owned
/// vector. Null with a length of `0` is the empty run; null with any other
/// length yields `None`.
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
    if len == 0 {
        return Some(Vec::new());
    }
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, -1);
    // SAFETY: the caller guarantees `ptr` is a `Box<[u8]>` of `len` bytes
    // transferred to us.
    Some(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) }.into_vec())
}

/// The body of `{prefix}_free_bytes`: release a run returned by this runtime
/// (a string, bytes, or a value buffer) or allocated with [`alloc`]. A null
/// `ptr` or a `len` of `0` is a no-op.
///
/// # Safety
///
/// `ptr` must be null or a pointer this runtime returned together with
/// `len` (or [`alloc`] returned for `len`), released exactly once.
pub unsafe fn free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    crate::abi::leak::track(crate::abi::leak::ALLOCATIONS, -1);
    // SAFETY: `bytes_into_raw` produced `ptr` from a `Box<[u8]>` of `len`
    // bytes, and the caller hands it back exactly once.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_roundtrip() {
        let mut len = 0usize;
        let ptr = unsafe { lower_bytes(vec![1u8, 2, 3, 4], &mut len) };
        assert_eq!(len, 4);
        assert_eq!(unsafe { lift_bytes(ptr, len) }, Some(vec![1, 2, 3, 4]));
        unsafe { free_bytes(ptr.cast_mut(), len) };
    }

    #[test]
    fn empty_is_null_and_null_is_empty() {
        let mut len = 99usize;
        let ptr = unsafe { lower_bytes(Vec::new(), &mut len) };
        assert!(ptr.is_null());
        assert_eq!(len, 0);
        assert_eq!(unsafe { lift_str(std::ptr::null(), 0) }, Some(""));
        assert_eq!(unsafe { lift_bytes(std::ptr::null(), 0) }, Some(vec![]));
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
        let ptr = unsafe { lower_string(text.to_string(), &mut len) };
        assert_eq!(unsafe { lift_string(ptr, len) }.as_deref(), Some(text));
        unsafe { free_bytes(ptr.cast_mut(), len) };
    }

    #[test]
    fn alloc_runs_free_and_adopt() {
        assert!(alloc(0).is_null());
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
