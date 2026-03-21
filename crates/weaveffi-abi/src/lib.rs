//! C ABI runtime: error struct, memory helpers, and utility functions.
#![allow(non_camel_case_types)]
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

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
