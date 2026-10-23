//! The `weaveffi::export_runtime!()` surface: every runtime symbol exists
//! under the crate's own prefix (this test crate is `export_runtime`), with
//! the C signature the generated header declares, and behaves as specified.

#![allow(unsafe_code)]

use std::ptr;

use weaveffi::abi::{self, FfiCancelToken, FfiError};

weaveffi::export_runtime!();

#[test]
fn every_runtime_symbol_has_its_c_signature() {
    let _: extern "C" fn() -> u32 = export_runtime_abi_version;
    let _: unsafe extern "C" fn(*mut FfiError, i32, *const u8, usize) = export_runtime_error_set;
    let _: unsafe extern "C" fn(*mut FfiError, *const u8, usize) = export_runtime_error_set_payload;
    let _: extern "C" fn(usize) -> *mut u8 = export_runtime_alloc;
    let _: unsafe extern "C" fn(*mut FfiError) = export_runtime_error_clear;
    let _: unsafe extern "C" fn(*mut FfiError) = export_runtime_error_free;
    let _: unsafe extern "C" fn(*mut u8, usize) = export_runtime_free_bytes;
    let _: extern "C" fn() -> *mut FfiCancelToken = export_runtime_cancel_token_create;
    let _: unsafe extern "C" fn(*mut FfiCancelToken) = export_runtime_cancel_token_cancel;
    let _: unsafe extern "C" fn(*const FfiCancelToken) -> bool =
        export_runtime_cancel_token_is_cancelled;
    let _: unsafe extern "C" fn(*mut FfiCancelToken) = export_runtime_cancel_token_destroy;
    let _: extern "C" fn(i32) -> u64 = export_runtime_debug_live;
    assert_eq!(export_runtime_abi_version(), 5);
}

#[test]
fn error_set_copies_and_clear_frees() {
    let mut err = FfiError::default();
    // The message is a length-delimited run, not NUL-terminated: a slice of
    // a larger buffer works, and an interior NUL survives.
    let backing = b"consumer\0failure, and more".to_vec();
    unsafe { export_runtime_error_set(&mut err, abi::FOREIGN_ERROR_CODE, backing.as_ptr(), 16) };
    drop(backing);
    assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(err.message_len, 16);
    assert_eq!(
        err.message_ptr as usize % abi::RUN_ALIGN,
        0,
        "an 8-aligned run"
    );
    assert_eq!(unsafe { err.message_str() }, Some("consumer\0failure"));
    unsafe { export_runtime_error_clear(&mut err) };
    assert_eq!(err.code, 0);
    assert!(err.message_ptr.is_null());
    assert_eq!(err.message_len, 0);
    unsafe {
        export_runtime_error_set(ptr::null_mut(), 1, ptr::null(), 0);
        export_runtime_error_clear(ptr::null_mut());
        export_runtime_error_free(ptr::null_mut());
    }
}

#[test]
fn error_set_payload_copies_the_run() {
    let mut err = FfiError::default();
    let fields = abi::encode_value(&"key".to_string());
    unsafe {
        export_runtime_error_set(&mut err, 1, ptr::null(), 0);
        export_runtime_error_set_payload(&mut err, fields.as_ptr(), fields.len());
    }
    drop(fields);
    assert_eq!(
        unsafe { abi::decode_value::<String>(err.payload()) }.unwrap(),
        "key"
    );
    unsafe { export_runtime_error_clear(&mut err) };
    assert!(err.payload_ptr.is_null());
}

#[test]
fn free_bytes_releases_returned_and_allocated_runs() {
    let (p, len) = abi::bytes_into_raw(&[1, 2, 3]);
    let staged = export_runtime_alloc(16);
    assert!(!staged.is_null());
    assert_eq!(staged as usize % abi::RUN_ALIGN, 0, "an 8-aligned run");
    assert!(export_runtime_alloc(0).is_null());
    unsafe {
        export_runtime_free_bytes(p.cast_mut(), len);
        export_runtime_free_bytes(staged, 16);
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
fn debug_live_reports_whether_it_counts() {
    // This crate's tests enable `leak-check`, so kind -1 reads 1.
    assert_eq!(export_runtime_debug_live(-1), 1);
    assert_eq!(export_runtime_debug_live(-2), 0);
    assert_eq!(export_runtime_debug_live(99), 0);
}
