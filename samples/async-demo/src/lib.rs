//! Async-demo sample cdylib used to exercise WeaveFFI's `async: true` function
//! code generation across all targets.
//!
//! The producer writes plain `async fn`s; the `#[weaveffi::module]` expansion
//! emits a launcher for each (running the future to completion on a worker
//! thread, then firing the host completion callback exactly once). A small
//! RAII `ActiveGuard` keeps `active_callbacks` honest: it counts task bodies
//! that are in flight and returns to zero once every spawned body has
//! completed or been cancelled.
//!
//! `wait` is cancellable for real: it waits on a timer until its timeout,
//! and cancelling its token completes the call with the cancelled code (`-5`)
//! right away, dropping the pending timer.

/// Async/await and cancellation demo across WeaveFFI's async-capable targets.
#[weaveffi::module]
pub mod tasks {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::task::{Context, Poll};

    /// The task module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// task name must not be empty
        InvalidName = 1,
    }

    impl std::fmt::Display for TaskError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("task name must not be empty")
        }
    }

    static NEXT_TASK_ID: AtomicI64 = AtomicI64::new(1);
    static ACTIVE_CALLBACKS: AtomicI64 = AtomicI64::new(0);

    /// RAII counter for in-flight async task bodies: increments on construction
    /// and decrements on drop. Because the `#[weaveffi::module]` expansion drops
    /// the future (and thus this guard) just before invoking the completion
    /// callback, `active_callbacks` is back to zero by the time a caller
    /// observes the callback.
    struct ActiveGuard;

    impl ActiveGuard {
        fn new() -> Self {
            ACTIVE_CALLBACKS.fetch_add(1, Ordering::SeqCst);
            ActiveGuard
        }
    }

    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            ACTIVE_CALLBACKS.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn next_id() -> i64 {
        NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed)
    }

    /// The by-value result an async task completes with.
    #[weaveffi::record]
    #[derive(Debug, Clone)]
    pub struct TaskResult {
        /// The id assigned to the completed task.
        pub id: i64,
        /// A human-readable completion message.
        pub value: String,
        /// Whether the task succeeded.
        pub success: bool,
    }

    /// Run a single named task, completing with its `TaskResult`. An empty
    /// name is rejected with [`TaskError::InvalidName`].
    #[weaveffi::export]
    pub async fn run_task(name: String) -> Result<TaskResult, TaskError> {
        let _guard = ActiveGuard::new();
        if name.is_empty() {
            return Err(TaskError::InvalidName);
        }
        Ok(TaskResult {
            id: next_id(),
            value: format!("completed: {name}"),
            success: true,
        })
    }

    /// Run a batch of named tasks, completing with one `TaskResult` per name.
    #[weaveffi::export]
    pub async fn run_batch(names: Vec<String>) -> Vec<TaskResult> {
        let _guard = ActiveGuard::new();
        names
            .into_iter()
            .map(|name| TaskResult {
                id: next_id(),
                value: format!("completed: {name}"),
                success: true,
            })
            .collect()
    }

    /// A timer future that needs no async runtime: the first poll starts a
    /// thread that sleeps until the deadline and then wakes the task.
    struct Delay {
        #[cfg(not(target_arch = "wasm32"))]
        deadline: std::time::Instant,
        waker: Option<std::sync::Arc<std::sync::Mutex<std::task::Waker>>>,
    }

    impl Delay {
        fn new(ms: u64) -> Self {
            // `wasm32-unknown-unknown` has no clock (`Instant::now` traps), so
            // there the delay completes on its first poll.
            #[cfg(target_arch = "wasm32")]
            let _ = ms;
            Self {
                #[cfg(not(target_arch = "wasm32"))]
                deadline: std::time::Instant::now() + std::time::Duration::from_millis(ms),
                waker: None,
            }
        }
    }

    impl Future for Delay {
        type Output = ();

        #[cfg(target_arch = "wasm32")]
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            // `wasm32-unknown-unknown` has neither threads nor a clock to
            // sleep on, so the timeout elapses at once there.
            let _ = &self.waker;
            Poll::Ready(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if std::time::Instant::now() >= self.deadline {
                return Poll::Ready(());
            }
            match &self.waker {
                // Keep the waker current in case the executor moved the task.
                Some(slot) => cx.waker().clone_into(&mut slot.lock().unwrap()),
                None => {
                    let slot = std::sync::Arc::new(std::sync::Mutex::new(cx.waker().clone()));
                    let deadline = self.deadline;
                    let wake = std::sync::Arc::clone(&slot);
                    std::thread::spawn(move || {
                        std::thread::sleep(
                            deadline.saturating_duration_since(std::time::Instant::now()),
                        );
                        wake.lock().unwrap().wake_by_ref();
                    });
                    self.waker = Some(slot);
                }
            }
            Poll::Pending
        }
    }

    /// Wait `timeout_ms` milliseconds, then complete with the milliseconds
    /// waited. Cancelling the token first completes the call with the
    /// cancelled code instead: the runtime drops this future, timer and all.
    #[weaveffi::export]
    #[weaveffi::cancellable]
    pub async fn wait(timeout_ms: i64, cancel: weaveffi::CancelToken) -> i64 {
        let _guard = ActiveGuard::new();
        let _ = cancel;
        let ms = u64::try_from(timeout_ms).unwrap_or(0);
        Delay::new(ms).await;
        timeout_ms.max(0)
    }

    /// Complete immediately with `n`. Drives the async stress examples, which
    /// verify the per-target wrapper pins the caller's context and callback for
    /// the duration of the call.
    #[weaveffi::export]
    pub async fn run_n_tasks(n: i32) -> i32 {
        let _guard = ActiveGuard::new();
        n
    }

    /// The number of async task bodies currently in flight; returns to zero
    /// once every outstanding task has completed.
    #[weaveffi::export]
    pub fn active_callbacks() -> i64 {
        ACTIVE_CALLBACKS.load(Ordering::SeqCst)
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::tasks::*;
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use weaveffi::abi::{self, FfiError};

    type TaskCbMsg = (i32, Option<TaskResult>);
    type BatchCbMsg = (bool, Vec<TaskResult>);

    /// The error code of a consumer-owned async error, which is released.
    fn take_code(err: *mut FfiError) -> i32 {
        if err.is_null() {
            return 0;
        }
        let code = unsafe { (*err).code };
        unsafe { crate::async_demo_error_free(err) };
        code
    }

    /// A buffered async result is an owned `(ptr, len)` value buffer: decode
    /// it, then release it with `{prefix}_free_bytes`.
    fn take_buffer<T: abi::BufferValue>(ptr: *const u8, len: usize) -> Option<T> {
        if ptr.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let value = abi::decode_value::<T>(bytes).expect("well-formed value buffer");
        unsafe { crate::async_demo_free_bytes(ptr.cast_mut(), len) };
        Some(value)
    }

    extern "C" fn task_callback(
        context: *mut c_void,
        err: *mut FfiError,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<TaskCbMsg>) };
        let _ = tx.send((take_code(err), take_buffer(result_ptr, result_len)));
    }

    extern "C" fn batch_callback(
        context: *mut c_void,
        err: *mut FfiError,
        results_ptr: *const u8,
        results_len: usize,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<BatchCbMsg>) };
        let had_error = take_code(err) != 0;
        let results = take_buffer(results_ptr, results_len).unwrap_or_default();
        let _ = tx.send((had_error, results));
    }

    extern "C" fn i32_callback(context: *mut c_void, err: *mut FfiError, result: i32) {
        let tx = unsafe { &*(context as *const mpsc::Sender<(i32, i32)>) };
        let _ = tx.send((take_code(err), result));
    }

    extern "C" fn i64_callback(context: *mut c_void, err: *mut FfiError, result: i64) {
        let tx = unsafe { &*(context as *const mpsc::Sender<(i32, i64)>) };
        let _ = tx.send((take_code(err), result));
    }

    /// Intentionally leak a callback-context box.
    ///
    /// The completion callback runs on a worker thread, so a worker may still
    /// be inside the callback's `send` when the test's `recv` returns (the
    /// receiver unblocks as soon as the message is queued, before `send`
    /// finishes). Reclaiming the box here would free the `Sender` out from
    /// under that in-flight `send`, so the test leaks the context instead;
    /// the OS reclaims the memory at process exit.
    fn leak_ctx<T>(ptr: *mut T) {
        std::mem::forget(unsafe { Box::from_raw(ptr) });
    }

    fn run_task(name: &str) -> TaskCbMsg {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        unsafe {
            async_demo_tasks_run_task(name.as_ptr(), name.len(), task_callback, tx_ptr.cast())
        };
        let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        out
    }

    #[test]
    fn run_task_calls_callback() {
        let (code, result) = run_task("test-task");
        assert_eq!(code, 0);
        let r = result.expect("success path passes a result buffer");
        assert!(r.id > 0);
        assert!(r.success);
        assert!(r.value.contains("test-task"));
    }

    #[test]
    fn run_task_empty_name_reports_invalid_name() {
        let (code, result) = run_task("");
        assert_eq!(code, 1, "TaskError::InvalidName's declared code");
        assert!(result.is_none(), "error path passes a null result buffer");
    }

    #[test]
    fn run_task_invalid_name_reports_marshal_error_through_the_callback() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        // The launcher has no out_err slot, so an argument that fails to lift
        // (a null pointer with a length) is reported through the completion
        // callback with the reserved marshalling code.
        unsafe { async_demo_tasks_run_task(std::ptr::null(), 3, task_callback, tx_ptr.cast()) };
        let (code, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert_eq!(code, abi::MARSHAL_ERROR_CODE);
        assert!(result.is_none());
    }

    fn run_batch(names: &[&str]) -> BatchCbMsg {
        let (tx, rx) = mpsc::channel::<BatchCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        // The list-of-strings parameter is buffered: the launcher copies the
        // bytes before returning, so the local buffer only needs to outlive
        // the call.
        let names = abi::encode_value(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        unsafe {
            async_demo_tasks_run_batch(names.as_ptr(), names.len(), batch_callback, tx_ptr.cast())
        };
        let out = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        leak_ctx(tx_ptr);
        out
    }

    #[test]
    fn run_batch_processes_sequentially() {
        let (had_error, results) = run_batch(&["task-a", "task-b", "task-c"]);
        assert!(!had_error);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.id > 0 && r.success));
        assert!(results[0].value.contains("task-a"));
        assert!(results[2].value.contains("task-c"));
    }

    #[test]
    fn run_batch_empty_names() {
        let (had_error, results) = run_batch(&[]);
        assert!(!had_error);
        assert!(results.is_empty());
    }

    #[test]
    fn task_result_buffer_round_trip() {
        for result in [
            TaskResult {
                id: 42,
                value: "hello".to_string(),
                success: true,
            },
            TaskResult {
                id: 1,
                value: "fail".to_string(),
                success: false,
            },
        ] {
            let back = abi::decode_value::<TaskResult>(&abi::encode_value(&result)).unwrap();
            assert_eq!(back.id, result.id);
            assert_eq!(back.value, result.value);
            assert_eq!(back.success, result.success);
        }
    }

    #[test]
    fn run_n_tasks_invokes_callback_with_n() {
        let (tx, rx) = mpsc::channel::<(i32, i32)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        unsafe { async_demo_tasks_run_n_tasks(7, i32_callback, tx_ptr.cast()) };
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (0, 7));
        leak_ctx(tx_ptr);
    }

    fn launch_wait(timeout_ms: i64, token: *mut abi::FfiCancelToken) -> mpsc::Receiver<(i32, i64)> {
        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        unsafe { async_demo_tasks_wait(timeout_ms, token, i64_callback, tx_ptr.cast()) };
        leak_ctx(tx_ptr);
        rx
    }

    #[test]
    fn wait_completes_after_its_timeout() {
        let token = crate::async_demo_cancel_token_create();
        let rx = launch_wait(20, token);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (0, 20));
        unsafe { crate::async_demo_cancel_token_destroy(token) };
    }

    #[test]
    fn cancelling_wait_completes_with_the_cancelled_code() {
        let token = crate::async_demo_cancel_token_create();
        let start = Instant::now();
        let rx = launch_wait(60_000, token);
        std::thread::sleep(Duration::from_millis(20));
        unsafe {
            crate::async_demo_cancel_token_cancel(token);
            crate::async_demo_cancel_token_destroy(token);
        }
        let (code, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(code, abi::CANCELLED_ERROR_CODE);
        assert_eq!(result, 0);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "didn't wait for the timeout"
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "completes once"
        );
    }

    #[test]
    fn a_token_cancelled_before_launch_never_runs_the_body() {
        let token = crate::async_demo_cancel_token_create();
        unsafe { crate::async_demo_cancel_token_cancel(token) };
        let rx = launch_wait(60_000, token);
        unsafe { crate::async_demo_cancel_token_destroy(token) };
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (abi::CANCELLED_ERROR_CODE, 0)
        );
    }

    #[test]
    fn active_callbacks_returns_to_zero() {
        let mut err = FfiError::default();
        let (tx, rx) = mpsc::channel::<(i32, i32)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        for i in 0..16 {
            unsafe { async_demo_tasks_run_n_tasks(i, i32_callback, tx_ptr.cast()) };
        }
        for _ in 0..16 {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        leak_ctx(tx_ptr);

        for _ in 0..50 {
            if unsafe { async_demo_tasks_active_callbacks(&mut err) } == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(unsafe { async_demo_tasks_active_callbacks(&mut err) }, 0);
    }
}
