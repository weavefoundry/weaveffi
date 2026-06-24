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
//!   into an owned Rust value for the duration of a call; they never take
//!   ownership of caller memory.
//! * **lower** functions (`Rust -> c_*`) hand an owned, heap-allocated value to
//!   the foreign caller, who later releases it through the matching
//!   `weaveffi_free_*` / `*_destroy` entry point.
//!
//! The lowering allocations mirror the ones the C header documents: strings via
//! [`string_to_c_ptr`] (freed with
//! `weaveffi_free_string`), byte and element buffers as a boxed slice (freed
//! with `weaveffi_free_bytes`), and opaque objects as `Box::into_raw` (freed
//! with the type's `_destroy`).

use std::os::raw::c_char;

use crate::{c_ptr_to_string, string_to_c_ptr};

// ── Lifting: C ABI slot -> owned Rust value ──────────────────────────────

/// Lift an optional UTF-8 string slot: a null pointer becomes `None`, a valid
/// pointer becomes `Some(String)`, and invalid UTF-8 becomes `None`.
///
/// This is the marshalling for an `Option<String>` / `string?` parameter; it is
/// intentionally identical to [`c_ptr_to_string`],
/// which already returns `None` for a null pointer.
#[must_use]
pub fn lift_opt_string(ptr: *const c_char) -> Option<String> {
    c_ptr_to_string(ptr)
}

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

/// Copy a foreign array of `Copy` scalars (`ptr` + `len`) into an owned `Vec<T>`.
///
/// A null `ptr` (or `len == 0`) yields an empty vector. This is the marshalling
/// for a `[scalar]` list parameter (`Vec<i32>`, `Vec<f64>`, and so on).
///
/// # Safety
///
/// When `ptr` is non-null it must point to at least `len` initialized values of
/// type `T` that stay valid for the duration of the call.
#[must_use]
pub unsafe fn lift_scalar_vec<T: Copy>(ptr: *const T, len: usize) -> Vec<T> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: caller guarantees `ptr` covers `len` initialized `T` values.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Copy a foreign array of C strings (`ptr` + `len`) into an owned
/// `Vec<String>`, mapping any null or non-UTF-8 element to an empty string.
///
/// This is the marshalling for a `[string]` list parameter.
///
/// # Safety
///
/// When `ptr` is non-null it must point to at least `len` initialized
/// `*const c_char` elements, each either null or a valid NUL-terminated string,
/// all staying valid for the duration of the call.
#[must_use]
pub unsafe fn lift_string_vec(ptr: *const *const c_char, len: usize) -> Vec<String> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: caller guarantees `ptr` covers `len` string pointers.
    let slots = unsafe { std::slice::from_raw_parts(ptr, len) };
    slots
        .iter()
        .map(|&p| c_ptr_to_string(p).unwrap_or_default())
        .collect()
}

/// Copy a foreign array of opaque object pointers (`ptr` + `len`) into an owned
/// `Vec<T>` by cloning each referenced value; null elements are skipped.
///
/// This is the marshalling for a `[Struct]` (list-of-object) parameter, where
/// each element crosses the ABI as a `const T*` into the producer's heap. The
/// returned vector owns clones, so the caller's objects are left untouched.
///
/// # Safety
///
/// When `ptr` is non-null it must point to at least `len` initialized
/// `*const T` elements, each either null or pointing to a valid `T` that stays
/// valid for the duration of the call.
#[must_use]
pub unsafe fn lift_ptr_vec<T: Clone>(ptr: *const *const T, len: usize) -> Vec<T> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: caller guarantees `ptr` covers `len` object pointers.
    let slots = unsafe { std::slice::from_raw_parts(ptr, len) };
    slots
        .iter()
        .filter_map(|&p| {
            if p.is_null() {
                None
            } else {
                // SAFETY: each non-null `p` points to a valid `T` for the call.
                Some(unsafe { &*p }.clone())
            }
        })
        .collect()
}

/// Lift an optional `Copy` scalar slot: a null pointer becomes `None`, a valid
/// pointer is dereferenced into `Some(value)`.
///
/// This is the marshalling for an `Option<scalar>` / `i32?` parameter, which
/// the ABI passes as a (nullable) pointer to the scalar.
///
/// # Safety
///
/// When `ptr` is non-null it must point to one initialized value of type `T`
/// valid for the duration of the call.
#[must_use]
pub unsafe fn lift_opt_scalar<T: Copy>(ptr: *const T) -> Option<T> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` points to one initialized `T`.
    Some(unsafe { *ptr })
}

// ── Lowering: owned Rust value -> C ABI slot ─────────────────────────────

/// Lower an optional string return: `None` becomes a null pointer, `Some`
/// becomes a freshly allocated C string the caller frees with
/// `weaveffi_free_string`.
#[must_use]
pub fn lower_opt_string(value: Option<impl AsRef<str>>) -> *const c_char {
    match value {
        Some(s) => string_to_c_ptr(s),
        None => std::ptr::null(),
    }
}

/// Lower an owned byte buffer into a heap allocation the caller frees with
/// `weaveffi_free_bytes`, writing the element count through `out_len`.
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

/// Lower an owned vector of `Copy` scalars into a heap buffer, writing the
/// element count through `out_len` and returning the base pointer.
///
/// An empty vector yields a null pointer and a length of `0`. The buffer is a
/// boxed slice (freeable with `weaveffi_free_bytes` interpreting the element
/// stride), matching the `[scalar]` return convention.
///
/// # Safety
///
/// `out_len`, when non-null, must point to a writable `usize`.
pub unsafe fn lower_scalar_vec<T>(data: Vec<T>, out_len: *mut usize) -> *mut T {
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = data.len() };
    }
    if data.is_empty() {
        return std::ptr::null_mut();
    }
    let boxed = data.into_boxed_slice();
    Box::into_raw(boxed) as *mut T
}

/// Lower an owned vector of strings into a heap array of C string pointers,
/// writing the element count through `out_len`.
///
/// Each element is allocated with [`string_to_c_ptr`].
/// An empty vector yields a null pointer and a length of `0`.
///
/// # Safety
///
/// `out_len`, when non-null, must point to a writable `usize`.
pub unsafe fn lower_string_vec(data: Vec<String>, out_len: *mut usize) -> *mut *const c_char {
    let ptrs: Vec<*const c_char> = data.iter().map(string_to_c_ptr).collect();
    // SAFETY: forwards the same `out_len` contract to `lower_scalar_vec`.
    unsafe { lower_scalar_vec(ptrs, out_len) }
}

/// Lower an owned vector of opaque object pointers into a heap array, writing
/// the element count through `out_len`.
///
/// Each element is typically the result of `Box::into_raw`; the caller frees
/// each element with the object's `_destroy` entry point. An empty vector
/// yields a null pointer and a length of `0`.
///
/// # Safety
///
/// `out_len`, when non-null, must point to a writable `usize`.
pub unsafe fn lower_ptr_vec<T>(data: Vec<*mut T>, out_len: *mut usize) -> *mut *mut T {
    // SAFETY: forwards the same `out_len` contract to `lower_scalar_vec`.
    unsafe { lower_scalar_vec(data, out_len) }
}

/// Lower an optional `Copy` scalar return: `None` becomes a null pointer,
/// `Some` becomes a freshly boxed value the caller dereferences and then frees.
///
/// The pointer is mutable to match the ABI's `scalar*` return slot (the
/// optional-scalar [`lower_return`](https://docs.rs/weaveffi-core) shape); the
/// lifting side accepts it through the `*const T` coercion.
#[must_use]
pub fn lower_opt_scalar<T>(value: Option<T>) -> *mut T {
    match value {
        Some(v) => Box::into_raw(Box::new(v)),
        None => std::ptr::null_mut(),
    }
}

/// Write a producer-allocated map back through the three out-parameters the ABI
/// uses for a `{K:V}` return: parallel key/value base pointers and a length.
///
/// `keys` and `values` are already-lowered parallel buffers (built with the
/// `lower_*` helpers above). A null out-pointer is skipped, so partial
/// out-parameter sets are tolerated.
///
/// # Safety
///
/// Each non-null out-pointer must be writable for its pointee type.
pub unsafe fn write_map_out<K, V>(
    keys: *mut K,
    values: *mut V,
    len: usize,
    out_keys: *mut *mut K,
    out_values: *mut *mut V,
    out_len: *mut usize,
) {
    if !out_keys.is_null() {
        // SAFETY: caller guarantees `out_keys` is writable when non-null.
        unsafe { *out_keys = keys };
    }
    if !out_values.is_null() {
        // SAFETY: caller guarantees `out_values` is writable when non-null.
        unsafe { *out_values = values };
    }
    if !out_len.is_null() {
        // SAFETY: caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = len };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{free_bytes, free_string};

    #[test]
    fn opt_string_roundtrip() {
        assert_eq!(lift_opt_string(std::ptr::null()), None);
        let p = string_to_c_ptr("hi");
        assert_eq!(lift_opt_string(p), Some("hi".to_string()));
        free_string(p);
    }

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
    fn scalar_vec_roundtrip() {
        let data = vec![10i32, 20, 30];
        let mut len = 0usize;
        let ptr = unsafe { lower_scalar_vec(data.clone(), &mut len) };
        assert_eq!(len, 3);
        let back = unsafe { lift_scalar_vec(ptr, len) };
        assert_eq!(back, data);
        free_bytes(ptr as *mut u8, len * std::mem::size_of::<i32>());
    }

    #[test]
    fn string_vec_roundtrip() {
        let data = vec!["a".to_string(), "bb".to_string()];
        let mut len = 0usize;
        let ptr = unsafe { lower_string_vec(data.clone(), &mut len) };
        assert_eq!(len, 2);
        let back = unsafe { lift_string_vec(ptr, len) };
        assert_eq!(back, data);
        // Free each element and the array.
        let slots = unsafe { std::slice::from_raw_parts(ptr, len) };
        for &p in slots {
            free_string(p);
        }
        free_bytes(ptr as *mut u8, len * std::mem::size_of::<*const c_char>());
    }

    #[test]
    fn opt_scalar_roundtrip() {
        assert_eq!(unsafe { lift_opt_scalar::<i32>(std::ptr::null()) }, None);
        let p = lower_opt_scalar(Some(7i32));
        assert_eq!(unsafe { lift_opt_scalar(p) }, Some(7));
        unsafe { drop(Box::from_raw(p)) };
        assert!(lower_opt_scalar::<i32>(None).is_null());
    }

    #[test]
    fn ptr_vec_lifts_and_skips_nulls() {
        #[derive(Clone, PartialEq, Debug)]
        struct Item {
            n: i32,
        }
        let a = Box::into_raw(Box::new(Item { n: 1 }));
        let b = Box::into_raw(Box::new(Item { n: 2 }));
        let slots: [*const Item; 3] = [a, std::ptr::null(), b];
        let v = unsafe { lift_ptr_vec(slots.as_ptr(), slots.len()) };
        assert_eq!(v, vec![Item { n: 1 }, Item { n: 2 }]);
        unsafe {
            drop(Box::from_raw(a));
            drop(Box::from_raw(b));
        }
        // Null base pointer yields an empty vector.
        let empty = unsafe { lift_ptr_vec::<Item>(std::ptr::null(), 3) };
        assert!(empty.is_empty());
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
