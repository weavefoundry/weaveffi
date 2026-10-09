//! An executor that drops an async call's future without finishing it must
//! still get exactly one completion, with the cancelled code. This runs in
//! its own test binary because it installs a process-wide spawner.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::abi::{self, FfiError};

#[weaveffi::module]
pub mod work {
    /// Would answer 42, if the executor ever ran it.
    #[weaveffi::export]
    pub async fn answer() -> i32 {
        42
    }
}

weaveffi::export_runtime!();

extern "C" fn done(ctx: *mut c_void, err: *mut FfiError, result: i32) {
    // Clone the sender before sending: the test may free the context as soon
    // as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<(i32, i32)>>() }.clone();
    let code = if err.is_null() {
        0
    } else {
        let code = unsafe { (*err).code };
        unsafe { abi::error_free(err) };
        code
    };
    tx.send((code, result)).unwrap();
}

#[test]
fn a_dropped_future_completes_with_the_cancelled_code_once() {
    weaveffi::set_spawner(drop::<weaveffi::BoxFuture>).unwrap();

    let (tx, rx) = mpsc::channel::<(i32, i32)>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    unsafe { work::dropped_future_work_answer(done, ctx) };
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        (abi::CANCELLED_ERROR_CODE, 0)
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<(i32, i32)>>()) });
}
