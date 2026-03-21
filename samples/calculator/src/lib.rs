#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_calculator_add(a: i32, b: i32, out_err: *mut weaveffi_error) -> i32 {
    abi::error_set_ok(out_err);
    a + b
}

#[no_mangle]
pub extern "C" fn weaveffi_calculator_mul(a: i32, b: i32, out_err: *mut weaveffi_error) -> i32 {
    abi::error_set_ok(out_err);
    a * b
}

#[no_mangle]
pub extern "C" fn weaveffi_calculator_div(a: i32, b: i32, out_err: *mut weaveffi_error) -> i32 {
    if b == 0 {
        abi::error_set(out_err, 2, "division by zero");
        return 0;
    }
    abi::error_set_ok(out_err);
    a / b
}

#[no_mangle]
pub extern "C" fn weaveffi_calculator_echo(
    ptr: *const u8,
    len: usize,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    // SAFETY: caller guarantees ptr is valid for len bytes
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    let s = match String::from_utf8(slice.to_vec()) {
        Ok(v) => v,
        Err(e) => {
            abi::error_set(out_err, 1, &format!("invalid UTF-8: {}", e));
            return std::ptr::null();
        }
    };
    abi::error_set_ok(out_err);
    abi::string_to_c_ptr(s)
}

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}
