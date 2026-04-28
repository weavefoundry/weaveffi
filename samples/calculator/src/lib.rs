//! Calculator sample cdylib used as a fixture for the WeaveFFI generators
//! and the end-to-end consumer examples.

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
    s: *const c_char,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    let input = match abi::c_ptr_to_string(s) {
        Some(v) => v,
        None => {
            abi::error_set(out_err, 1, "s is null or invalid UTF-8");
            return std::ptr::null();
        }
    };
    abi::error_set_ok(out_err);
    abi::string_to_c_ptr(&input)
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
