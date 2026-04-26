//! C ABI runtime: error struct, memory helpers, and utility functions.
#![allow(non_camel_case_types)]
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod arena;

use std::alloc::{alloc as sys_alloc, dealloc as sys_dealloc, Layout};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

/// Layout used by [`weaveffi_alloc`] / [`weaveffi_free`]: alignment 1 for raw byte arrays.
fn byte_layout(size: usize) -> Option<Layout> {
    if size == 0 {
        return None;
    }
    Layout::from_size_align(size, 1).ok()
}

/// Allocate `size` bytes of raw storage with alignment 1, returning an owned pointer.
///
/// Returns a null pointer when `size` is zero or allocation fails. The caller
/// must release the buffer with [`weaveffi_free`] using the same `size`.
#[no_mangle]
pub extern "C" fn weaveffi_alloc(size: usize) -> *mut u8 {
    let Some(layout) = byte_layout(size) else {
        return ptr::null_mut();
    };
    // SAFETY: `layout` has non-zero size, which is the requirement for `std::alloc::alloc`.
    unsafe { sys_alloc(layout) }
}

/// Free a buffer previously returned by [`weaveffi_alloc`].
///
/// `size` must match the size passed to `weaveffi_alloc`. A null `ptr` or zero
/// `size` is a no-op so foreign callers can safely forward defaults.
///
/// # Safety
///
/// `ptr` must have been returned by [`weaveffi_alloc`] with the same `size` and
/// must not be used after this call.
#[no_mangle]
pub extern "C" fn weaveffi_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    let Some(layout) = byte_layout(size) else {
        return;
    };
    // SAFETY: caller guarantees `ptr` came from `weaveffi_alloc` with matching `size`.
    unsafe { sys_dealloc(ptr, layout) };
}

/// Public opaque handle type exposed to foreign callers.
pub type weaveffi_handle_t = u64;

/// Error struct passed across the C ABI boundary.
///
/// # Safety
///
/// - `message` is a NUL-terminated UTF-8 C string allocated by Rust and must be
///   released by calling `weaveffi_error_clear` or `weaveffi_free_string`.
/// - This struct must not be copied while it owns a message pointer, as that
///   would lead to double-free on clear.
#[repr(C)]
#[derive(Debug)]
pub struct weaveffi_error {
    pub code: i32,
    pub message: *const c_char,
}

impl Default for weaveffi_error {
    fn default() -> Self {
        Self {
            code: 0,
            message: ptr::null(),
        }
    }
}

/// Set the error to OK (code = 0) and free any prior message.
pub fn error_set_ok(out_err: *mut weaveffi_error) {
    if out_err.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let err = unsafe { &mut *out_err };
    if !err.message.is_null() {
        // SAFETY: message was allocated via `CString::into_raw` in this module
        unsafe { drop(CString::from_raw(err.message as *mut c_char)) };
    }
    err.code = 0;
    err.message = ptr::null();
}

/// Populate an error with the given code and message (copying message).
pub fn error_set(out_err: *mut weaveffi_error, code: i32, message: &str) {
    if out_err.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let err = unsafe { &mut *out_err };
    if !err.message.is_null() {
        // SAFETY: message was allocated via `CString::into_raw` in this module
        unsafe { drop(CString::from_raw(err.message as *mut c_char)) };
    }
    err.code = code;
    let owned_message = message.replace('\0', "");
    let cstr = CString::new(owned_message).expect("CString::new sanitized input");
    err.message = cstr.into_raw();
}

/// Convenience adapter: map a `Result<T, E>` to `Option<T>` by writing into `out_err`.
pub fn result_to_out_err<T, E: std::fmt::Display>(
    result: Result<T, E>,
    out_err: *mut weaveffi_error,
) -> Option<T> {
    match result {
        Ok(value) => {
            error_set_ok(out_err);
            Some(value)
        }
        Err(e) => {
            error_set(out_err, -1, &e.to_string());
            None
        }
    }
}

/// Allocate a new C string from a Rust string, returning an owned pointer.
/// Caller must later free with `weaveffi_free_string` or `weaveffi_error_clear`.
pub fn string_to_c_ptr(s: impl AsRef<str>) -> *const c_char {
    let s = s.as_ref();
    let sanitized = if s.as_bytes().contains(&0) {
        s.replace('\0', "")
    } else {
        s.to_owned()
    };
    let cstr = CString::new(sanitized).expect("string_to_c_ptr: unexpected NUL after sanitization");
    cstr.into_raw()
}

/// Free a C string previously allocated by this runtime.
pub fn free_string(ptr: *const c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: pointer must be returned from `CString::into_raw`
    unsafe { drop(CString::from_raw(ptr as *mut c_char)) };
}

/// Free a byte buffer previously allocated by Rust and returned to foreign code.
pub fn free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: reconstructs the original Box<[u8]> for deallocation
    unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len))) };
}

/// Clear an error by freeing any message and zeroing fields.
pub fn error_clear(err: *mut weaveffi_error) {
    error_set_ok(err);
}

/// Opaque cancellation token passed across the C ABI boundary.
///
/// Foreign callers obtain a token via `weaveffi_cancel_token_create`, signal
/// cancellation with `weaveffi_cancel_token_cancel`, and release it with
/// `weaveffi_cancel_token_destroy`.
#[repr(C)]
pub struct weaveffi_cancel_token {
    cancelled: AtomicBool,
}

/// Allocate a new cancel token. The caller owns the returned pointer and must
/// eventually call `weaveffi_cancel_token_destroy`.
pub fn cancel_token_create() -> *mut weaveffi_cancel_token {
    Box::into_raw(Box::new(weaveffi_cancel_token {
        cancelled: AtomicBool::new(false),
    }))
}

/// Signal cancellation on the token (thread-safe).
pub fn cancel_token_cancel(token: *mut weaveffi_cancel_token) {
    if token.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let t = unsafe { &*token };
    t.cancelled.store(true, Ordering::Release);
}

/// Check whether the token has been cancelled (thread-safe).
pub fn cancel_token_is_cancelled(token: *const weaveffi_cancel_token) -> bool {
    if token.is_null() {
        return false;
    }
    // SAFETY: pointer checked for null above
    let t = unsafe { &*token };
    t.cancelled.load(Ordering::Acquire)
}

/// Destroy a cancel token previously created by `cancel_token_create`.
pub fn cancel_token_destroy(token: *mut weaveffi_cancel_token) {
    if token.is_null() {
        return;
    }
    // SAFETY: pointer was returned from `Box::into_raw` in `cancel_token_create`
    unsafe { drop(Box::from_raw(token)) };
}

/// Convert a NUL-terminated C string pointer to an owned `String`.
/// Returns `None` if `ptr` is null or not valid UTF-8.
pub fn c_ptr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` points to a NUL-terminated string
    let c = unsafe { CStr::from_ptr(ptr) };
    c.to_str().ok().map(|s| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_roundtrip_and_free() {
        let ptr = string_to_c_ptr("hello world");
        assert!(!ptr.is_null());
        let recovered = c_ptr_to_string(ptr).unwrap();
        assert_eq!(recovered, "hello world");
        free_string(ptr);
    }

    #[test]
    fn free_string_null_is_safe() {
        free_string(ptr::null());
    }

    #[test]
    fn free_bytes_null_is_safe() {
        free_bytes(ptr::null_mut(), 0);
    }

    #[test]
    fn bytes_alloc_and_free() {
        let data: Vec<u8> = vec![1, 2, 3, 4, 5];
        let len = data.len();
        let boxed = data.into_boxed_slice();
        let ptr = Box::into_raw(boxed) as *mut u8;
        free_bytes(ptr, len);
    }

    #[test]
    fn error_default_is_ok() {
        let err = weaveffi_error::default();
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_set_and_clear() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, -1, "something went wrong");
        assert_eq!(err.code, -1);
        assert!(!err.message.is_null());
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "something went wrong");
        error_clear(&mut err);
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_clear_null_is_safe() {
        error_clear(ptr::null_mut());
    }

    #[test]
    fn error_set_ok_frees_prior_message() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, 1, "first");
        error_set_ok(&mut err);
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_set_replaces_prior_message() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, 1, "first");
        error_set(&mut err, 2, "second");
        assert_eq!(err.code, 2);
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "second");
        error_clear(&mut err);
    }

    #[test]
    fn result_to_out_err_ok_path() {
        let mut err = weaveffi_error::default();
        let val: Result<i32, String> = Ok(42);
        let opt = result_to_out_err(val, &mut err);
        assert_eq!(opt, Some(42));
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn result_to_out_err_error_path() {
        let mut err = weaveffi_error::default();
        let val: Result<i32, String> = Err("bad input".to_string());
        let opt = result_to_out_err(val, &mut err);
        assert_eq!(opt, None);
        assert_eq!(err.code, -1);
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "bad input");
        error_clear(&mut err);
    }

    #[test]
    fn string_with_interior_nul_is_sanitized() {
        let ptr = string_to_c_ptr("hel\0lo");
        let recovered = c_ptr_to_string(ptr).unwrap();
        assert_eq!(recovered, "hello");
        free_string(ptr);
    }

    #[test]
    fn c_ptr_to_string_null_returns_none() {
        assert_eq!(c_ptr_to_string(ptr::null()), None);
    }

    #[test]
    fn cancel_token_lifecycle() {
        let token = cancel_token_create();
        assert!(!token.is_null());
        assert!(!cancel_token_is_cancelled(token));
        cancel_token_cancel(token);
        assert!(cancel_token_is_cancelled(token));
        cancel_token_destroy(token);
    }

    #[test]
    fn cancel_token_null_is_safe() {
        cancel_token_cancel(ptr::null_mut());
        assert!(!cancel_token_is_cancelled(ptr::null()));
        cancel_token_destroy(ptr::null_mut());
    }

    #[test]
    fn alloc_and_free_round_trip() {
        const SIZE: usize = 64;
        let ptr = weaveffi_alloc(SIZE);
        assert!(!ptr.is_null());

        // SAFETY: `ptr` points to `SIZE` bytes just allocated above.
        let buf = unsafe { std::slice::from_raw_parts_mut(ptr, SIZE) };
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = (i as u8) ^ 0xA5;
        }
        for (i, &value) in buf.iter().enumerate() {
            assert_eq!(value, (i as u8) ^ 0xA5);
        }

        weaveffi_free(ptr, SIZE);
    }

    #[test]
    fn alloc_zero_returns_null() {
        assert!(weaveffi_alloc(0).is_null());
    }

    #[test]
    fn free_null_is_safe() {
        weaveffi_free(ptr::null_mut(), 0);
        weaveffi_free(ptr::null_mut(), 32);
    }
}
