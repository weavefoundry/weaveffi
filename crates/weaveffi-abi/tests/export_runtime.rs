//! Smoke test for the `weaveffi_abi::export_runtime!()` macro.
//!
//! This integration test lives in a separate compilation unit (it is
//! linked as a Rust test binary, not a cdylib) so it cannot easily
//! observe the `#[no_mangle]` symbols' linker visibility, but it
//! *can* prove that the macro expands to valid Rust that compiles,
//! that the generated free functions are callable with realistic
//! arguments, and that the symbol names match what the WeaveFFI
//! generators emit.
//!
//! Two invocation guards keep us honest:
//! 1. Each `weaveffi_*` thunk is called directly from the test.
//! 2. We take a function-pointer reference to every symbol the macro
//!    is supposed to expose, so any missing thunk turns into a compile
//!    error here rather than a baffling link error in a downstream
//!    cdylib.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::ptr;

use weaveffi_abi::{self as abi, export_runtime, weaveffi_cancel_token, weaveffi_error};

export_runtime!();

#[test]
fn macro_emits_all_runtime_symbols() {
    // Compile-time guarantee: every symbol the C ABI generators expect
    // must be addressable from this test crate.
    let _f_string: extern "C" fn(*const std::os::raw::c_char) = weaveffi_free_string;
    let _f_bytes: extern "C" fn(*mut u8, usize) = weaveffi_free_bytes;
    let _f_err_clear: extern "C" fn(*mut weaveffi_error) = weaveffi_error_clear;
    let _f_tok_create: extern "C" fn() -> *mut weaveffi_cancel_token = weaveffi_cancel_token_create;
    let _f_tok_cancel: extern "C" fn(*mut weaveffi_cancel_token) = weaveffi_cancel_token_cancel;
    let _f_tok_is_cancelled: extern "C" fn(*const weaveffi_cancel_token) -> bool =
        weaveffi_cancel_token_is_cancelled;
    let _f_tok_destroy: extern "C" fn(*mut weaveffi_cancel_token) = weaveffi_cancel_token_destroy;

    let _f_arena_create: extern "C" fn() -> *mut abi::arena::HandleArena = weaveffi_arena_create;
    let _f_arena_register: extern "C" fn(
        *mut abi::arena::HandleArena,
        *mut std::ffi::c_void,
        unsafe extern "C" fn(*mut std::ffi::c_void),
    ) = weaveffi_arena_register;
    let _f_arena_destroy: extern "C" fn(*mut abi::arena::HandleArena) = weaveffi_arena_destroy;
}

#[test]
fn macro_free_string_thunk_round_trips() {
    let ptr = abi::string_to_c_ptr("from-macro-test");
    assert!(!ptr.is_null());
    weaveffi_free_string(ptr);
    weaveffi_free_string(ptr::null());
}

#[test]
fn macro_free_bytes_thunk_round_trips() {
    let data = vec![1u8, 2, 3, 4].into_boxed_slice();
    let len = data.len();
    let raw = Box::into_raw(data) as *mut u8;
    weaveffi_free_bytes(raw, len);
    weaveffi_free_bytes(ptr::null_mut(), 0);
}

#[test]
fn macro_error_clear_thunk_round_trips() {
    let mut err = weaveffi_error::default();
    abi::error_set(&mut err, 7, "from-macro-test");
    assert_eq!(err.code, 7);
    assert!(!err.message.is_null());

    weaveffi_error_clear(&mut err);
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());

    weaveffi_error_clear(ptr::null_mut());
}

#[test]
fn macro_cancel_token_thunks_round_trip() {
    let tok = weaveffi_cancel_token_create();
    assert!(!tok.is_null());
    assert!(!weaveffi_cancel_token_is_cancelled(tok));

    weaveffi_cancel_token_cancel(tok);
    assert!(weaveffi_cancel_token_is_cancelled(tok));

    weaveffi_cancel_token_destroy(tok);

    weaveffi_cancel_token_cancel(ptr::null_mut());
    assert!(!weaveffi_cancel_token_is_cancelled(ptr::null()));
    weaveffi_cancel_token_destroy(ptr::null_mut());
}

#[test]
fn macro_arena_thunks_round_trip() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn counting_dtor(ptr: *mut c_void) {
        let counter = ptr as *const AtomicUsize;
        (*counter).fetch_add(1, Ordering::SeqCst);
    }

    let counter = AtomicUsize::new(0);
    let counter_ptr = &counter as *const AtomicUsize as *mut c_void;

    let arena = weaveffi_arena_create();
    assert!(!arena.is_null());

    weaveffi_arena_register(arena, counter_ptr, counting_dtor);
    weaveffi_arena_register(arena, counter_ptr, counting_dtor);

    weaveffi_arena_destroy(arena);
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    weaveffi_arena_register(
        ptr::null_mut(),
        ptr::dangling_mut::<c_void>(),
        counting_dtor,
    );
    weaveffi_arena_destroy(ptr::null_mut());
}

#[test]
fn macro_thunks_keep_weaveffi_prefix_even_when_user_renames_imports() {
    // Re-import everything under a different alias to make sure the
    // macro is not silently capturing identifiers from the call site.
    use weaveffi_abi as renamed;

    let s = CString::new("alias-check").unwrap();
    let owned = renamed::string_to_c_ptr(s.to_str().unwrap());
    weaveffi_free_string(owned);
}
