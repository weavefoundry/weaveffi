//! The `weaveffi::export_runtime!()` surface: every runtime symbol exists
//! under the crate's own prefix (this test crate is `export_runtime`), with
//! the C signature the generated header declares, and behaves as specified.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use weaveffi::abi::{self, FfiCancelToken, FfiError};

weaveffi::export_runtime!();

#[test]
fn every_runtime_symbol_has_its_c_signature() {
    let _: extern "C" fn() -> u32 = export_runtime_abi_version;
    let _: unsafe extern "C" fn(*mut FfiError, i32, *const c_char) = export_runtime_error_set;
    let _: unsafe extern "C" fn(*mut FfiError) = export_runtime_error_clear;
    let _: unsafe extern "C" fn(*mut FfiError) = export_runtime_error_free;
    let _: unsafe extern "C" fn(*mut u8, usize) = export_runtime_free_bytes;
    let _: extern "C" fn() -> *mut FfiCancelToken = export_runtime_cancel_token_create;
    let _: unsafe extern "C" fn(*mut FfiCancelToken) = export_runtime_cancel_token_cancel;
    let _: unsafe extern "C" fn(*const FfiCancelToken) -> bool =
        export_runtime_cancel_token_is_cancelled;
    let _: unsafe extern "C" fn(*mut FfiCancelToken) = export_runtime_cancel_token_destroy;
    let _: extern "C" fn(i32) -> u64 = export_runtime_debug_live;
    assert_eq!(export_runtime_abi_version(), 3);
}

#[test]
fn error_set_copies_and_clear_frees() {
    let mut err = FfiError::default();
    let msg = CString::new("consumer failure").unwrap();
    unsafe { export_runtime_error_set(&mut err, abi::FOREIGN_ERROR_CODE, msg.as_ptr()) };
    drop(msg);
    assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(unsafe { err.message_str() }, Some("consumer failure"));
    unsafe { export_runtime_error_clear(&mut err) };
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());
    unsafe {
        export_runtime_error_set(ptr::null_mut(), 1, ptr::null());
        export_runtime_error_clear(ptr::null_mut());
        export_runtime_error_free(ptr::null_mut());
    }
}

#[test]
fn free_bytes_releases_returned_runs() {
    let (p, len) = abi::bytes_into_raw(vec![1, 2, 3]);
    unsafe {
        export_runtime_free_bytes(p.cast_mut(), len);
        export_runtime_free_bytes(ptr::null_mut(), 0);
    }
}

#[test]
fn cancel_tokens_round_trip() {
    let tok = export_runtime_cancel_token_create();
    assert!(!tok.is_null());
    unsafe {
        assert!(!export_runtime_cancel_token_is_cancelled(tok));
        export_runtime_cancel_token_cancel(tok);
        export_runtime_cancel_token_cancel(tok);
        assert!(export_runtime_cancel_token_is_cancelled(tok));
        export_runtime_cancel_token_destroy(tok);

        export_runtime_cancel_token_cancel(ptr::null_mut());
        assert!(!export_runtime_cancel_token_is_cancelled(ptr::null()));
        export_runtime_cancel_token_destroy(ptr::null_mut());
    }
}

#[test]
fn unknown_leak_kinds_read_zero() {
    assert_eq!(export_runtime_debug_live(-1), 0);
    assert_eq!(export_runtime_debug_live(99), 0);
}
