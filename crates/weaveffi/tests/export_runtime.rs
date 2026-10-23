//! The `weaveffi::export_runtime!()` surface: every runtime symbol exists
//! under the crate's own prefix (this test crate is `export_runtime`), with
//! the C signature the generated header declares, and behaves as specified.

#![allow(unsafe_code)]
// The `extern` block names the runtime's opaque cancel token behind a
// pointer, as the C header names an incomplete struct.
#![allow(improper_ctypes)]

use std::ptr;

use weaveffi::__rt::{self as abi, BufferReader, FfiCancelToken, FfiError, Run};

weaveffi::export_runtime!();

// The runtime symbols as a C consumer declares them.
unsafe extern "C" {
    fn export_runtime_abi_version() -> u32;
    fn export_runtime_error_set(err: *mut FfiError, code: i32, ptr: *const u8, len: usize);
    fn export_runtime_error_set_payload(err: *mut FfiError, ptr: *const u8, len: usize);
    fn export_runtime_error_clear(err: *mut FfiError);
    fn export_runtime_error_free(err: *mut FfiError);
    fn export_runtime_alloc(len: usize) -> *mut u8;
    fn export_runtime_run_free(run: Run);
    fn export_runtime_cancel_token_create() -> *mut FfiCancelToken;
    fn export_runtime_cancel_token_cancel(token: *mut FfiCancelToken);
    fn export_runtime_cancel_token_is_cancelled(token: *const FfiCancelToken) -> bool;
    fn export_runtime_cancel_token_destroy(token: *mut FfiCancelToken);
    fn export_runtime_debug_live(kind: i32) -> u64;
}

#[test]
fn the_runtime_reports_its_revision() {
    assert_eq!(unsafe { export_runtime_abi_version() }, 6);
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
    assert_eq!(err.message_ptr as usize % 8, 0, "an 8-aligned run");
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
fn error_set_replaces_what_the_slot_held() {
    let mut err = FfiError::default();
    unsafe {
        export_runtime_error_set(&mut err, 1, b"first".as_ptr(), 5);
        export_runtime_error_set(&mut err, 2, b"second".as_ptr(), 6);
    }
    assert_eq!(
        (err.code, unsafe { err.message_str() }),
        (2, Some("second"))
    );
    unsafe { export_runtime_error_clear(&mut err) };
}

#[test]
fn error_set_payload_copies_the_run() {
    let mut err = FfiError::default();
    let fields = abi::encode(|w| w.write_string("key"));
    unsafe {
        export_runtime_error_set(&mut err, 1, ptr::null(), 0);
        export_runtime_error_set_payload(&mut err, fields.as_ptr(), fields.len());
    }
    drop(fields);
    let mut r = BufferReader::new(err.payload());
    assert_eq!(r.read_string().unwrap(), "key");
    unsafe { export_runtime_error_clear(&mut err) };
    assert!(err.payload_ptr.is_null());
}

#[test]
fn run_free_releases_returned_and_allocated_runs() {
    let returned = Run::copy_of(&[1, 2, 3]);
    assert_eq!(returned.ptr as usize % 8, 0, "an 8-aligned run");
    let staged = unsafe { export_runtime_alloc(16) };
    assert!(!staged.is_null());
    assert_eq!(staged as usize % 8, 0, "an 8-aligned run");
    assert!(unsafe { export_runtime_alloc(0) }.is_null());
    unsafe {
        export_runtime_run_free(returned);
        export_runtime_run_free(Run {
            ptr: staged,
            len: 16,
        });
        export_runtime_run_free(Run::EMPTY);
    }
}

#[test]
fn cancel_tokens_round_trip() {
    unsafe {
        let tok = export_runtime_cancel_token_create();
        assert!(!tok.is_null());
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
    unsafe {
        assert_eq!(export_runtime_debug_live(-1), 1);
        assert_eq!(export_runtime_debug_live(-2), 0);
        assert_eq!(export_runtime_debug_live(99), 0);
    }
}
