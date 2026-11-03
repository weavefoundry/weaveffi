//! The `tokio` feature: exported `async fn`s run on Tokio, so a future may
//! use Tokio's timers and I/O. Outside a runtime the library starts its own;
//! inside one (a consumer calling from a Tokio thread) it uses the current
//! handle. Run with `cargo test -p weaveffi --features tokio`.

#![cfg(feature = "tokio")]
#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::__rt::{self as abi, FfiError, Run};

/// Sleep on a Tokio timer, then report the runtime thread's name.
#[weaveffi::export]
pub async fn nap(ms: u64) -> String {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    std::thread::current().name().unwrap_or("").to_string()
}

weaveffi::export_runtime!();

type Done = extern "C" fn(*mut c_void, *mut FfiError, Run);

unsafe extern "C" {
    fn tokio_nap(ms: u64, callback: Option<Done>, context: *mut c_void);
}

extern "C" fn done(ctx: *mut c_void, err: *mut FfiError, name: Run) {
    // Clone the sender before sending: the test may free the context as soon
    // as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<(i32, String)>>() }.clone();
    let code = if err.is_null() {
        0
    } else {
        let code = unsafe { (*err).code };
        unsafe { abi::error_free(err) };
        code
    };
    let text = unsafe { name.text() }.unwrap_or_default().to_string();
    unsafe { name.free() };
    tx.send((code, text)).unwrap();
}

fn call_nap(ms: u64) -> (i32, String) {
    let (tx, rx) = mpsc::channel::<(i32, String)>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    unsafe { tokio_nap(ms, Some(done), ctx) };
    let out = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    drop(unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<(i32, String)>>()) });
    out
}

#[test]
fn outside_a_runtime_the_library_starts_its_own() {
    assert_eq!(call_nap(5), (0, "weaveffi-async".to_string()));
}

#[test]
fn inside_a_runtime_the_current_handle_is_used() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .thread_name("consumer-runtime")
        .enable_all()
        .build()
        .unwrap();
    let (code, name) =
        rt.block_on(async { tokio::task::spawn_blocking(|| call_nap(5)).await.unwrap() });
    assert_eq!((code, name.as_str()), (0, "consumer-runtime"));
}
