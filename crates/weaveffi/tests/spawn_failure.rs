//! A spawner that fails (panics) completes the call with the generic code
//! and a message, exactly once. This runs in its own test binary because it
//! installs a process-wide spawner.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::abi::{self, FfiError};

#[weaveffi::module]
pub mod work {
    /// Would answer 42, if the executor took it.
    #[weaveffi::export]
    pub async fn answer() -> i32 {
        42
    }
}

extern "C" fn done(ctx: *mut c_void, err: *mut FfiError, result: i32) {
    // Clone the sender before sending: the test may free the context as soon
    // as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<(i32, String, i32)>>() }.clone();
    let (code, message) = unsafe { ((*err).code, (*err).message_str().unwrap_or("").to_string()) };
    unsafe { abi::error_free(err) };
    tx.send((code, message, result)).unwrap();
}

#[test]
fn a_failing_spawner_completes_with_the_generic_code_once() {
    weaveffi::set_spawner(|_fut: weaveffi::BoxFuture| panic!("executor is gone")).unwrap();

    let (tx, rx) = mpsc::channel::<(i32, String, i32)>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    unsafe { work::spawn_failure_work_answer(done, ctx) };
    let (code, message, result) = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!((code, result), (abi::GENERIC_ERROR_CODE, 0));
    assert!(message.contains("executor is gone"), "{message}");
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<(i32, String, i32)>>()) });
}
