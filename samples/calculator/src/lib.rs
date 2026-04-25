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
    s_ptr: *const u8,
    s_len: usize,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    if s_ptr.is_null() {
        abi::error_set(out_err, 1, "s is null");
        return std::ptr::null();
    }
    let slice = unsafe { std::slice::from_raw_parts(s_ptr, s_len) };
    let input = match std::str::from_utf8(slice) {
        Ok(v) => v,
        Err(_) => {
            abi::error_set(out_err, 1, "s is not valid UTF-8");
            return std::ptr::null();
        }
    };
    abi::error_set_ok(out_err);
    abi::string_to_c_ptr(input)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_roundtrips_ascii_byteslice() {
        let mut err = weaveffi_error::default();
        let input = "hello";
        let out = weaveffi_calculator_echo(input.as_ptr(), input.len(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(out).unwrap(), "hello");
        abi::free_string(out);
    }

    #[test]
    fn echo_roundtrips_multibyte_utf8() {
        let mut err = weaveffi_error::default();
        let input = "héllo→世界";
        let out = weaveffi_calculator_echo(input.as_ptr(), input.len(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(out).unwrap(), "héllo→世界");
        abi::free_string(out);
    }

    #[test]
    fn echo_handles_empty_string() {
        let mut err = weaveffi_error::default();
        let input = "";
        let out = weaveffi_calculator_echo(input.as_ptr(), input.len(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(out).unwrap(), "");
        abi::free_string(out);
    }

    #[test]
    fn echo_ignores_bytes_beyond_len() {
        let mut err = weaveffi_error::default();
        let input = "abcdef";
        let out = weaveffi_calculator_echo(input.as_ptr(), 3, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(out).unwrap(), "abc");
        abi::free_string(out);
    }

    #[test]
    fn echo_null_pointer_sets_error() {
        let mut err = weaveffi_error::default();
        let out = weaveffi_calculator_echo(std::ptr::null(), 0, &mut err);
        assert!(out.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn echo_invalid_utf8_sets_error() {
        let mut err = weaveffi_error::default();
        let bad: [u8; 3] = [0xFF, 0xFE, 0xFD];
        let out = weaveffi_calculator_echo(bad.as_ptr(), bad.len(), &mut err);
        assert!(out.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }
}
