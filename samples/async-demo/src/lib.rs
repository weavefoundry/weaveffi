//! Async-demo sample cdylib used to exercise WeaveFFI's `async: true` function
//! code generation across all targets.
//!
//! The producer writes plain `async fn`s; the `#[weaveffi::module]` expansion
//! emits the `_async` launcher for each (running the future to completion on a
//! worker thread, then firing the host completion callback). A small RAII
//! `ActiveGuard` keeps `active_callbacks` honest: it counts task bodies that
//! are in flight and returns to zero once every spawned body has completed.

/// Async/await and cancellation demo across WeaveFFI's async-capable targets.
#[weaveffi::module]
pub mod tasks {
    use std::sync::atomic::{AtomicI64, Ordering};

    /// The task module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// task name must not be empty
        InvalidName = 1,
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

    /// Best-effort cancel of a task by id. This demo has no long-running work
    /// to interrupt, so it always reports "not cancelled".
    #[weaveffi::export]
    pub fn cancel_task(id: i64) -> bool {
        let _ = id;
        false
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
    use std::ffi::CString;
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;
    use weaveffi::abi::{self, weaveffi_error};

    type TaskCbMsg = (i32, Option<TaskResult>);
    type BatchCbMsg = (bool, Vec<TaskResult>);

    /// A buffered async result arrives as a borrowed `(ptr, len)` value
    /// buffer the launcher frees after the callback returns, so the callback
    /// decodes it before sending.
    extern "C" fn task_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<TaskCbMsg>) };
        let code = if err.is_null() {
            0
        } else {
            unsafe { (*err).code }
        };
        let result = if result_ptr.is_null() {
            None
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(result_ptr, result_len) };
            Some(abi::decode_value::<TaskResult>(bytes).expect("well-formed TaskResult buffer"))
        };
        let _ = tx.send((code, result));
    }

    extern "C" fn batch_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        results_ptr: *const u8,
        results_len: usize,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<BatchCbMsg>) };
        let had_error = !err.is_null() && unsafe { (*err).code } != 0;
        // Decode the borrowed value buffer before returning; the launcher
        // frees it once the callback completes.
        let results = if results_ptr.is_null() {
            Vec::new()
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(results_ptr, results_len) };
            abi::decode_value::<Vec<TaskResult>>(bytes).expect("well-formed batch buffer")
        };
        let _ = tx.send((had_error, results));
    }

    extern "C" fn n_tasks_callback(context: *mut c_void, err: *mut weaveffi_error, result: i32) {
        let tx = unsafe { &*(context as *const mpsc::Sender<(bool, i32)>) };
        let had_error = !err.is_null() && unsafe { (*err).code } != 0;
        let _ = tx.send((had_error, result));
    }

    /// Intentionally leak a callback-context box.
    ///
    /// The `#[weaveffi::module]` async launchers invoke the completion callback
    /// on a detached worker thread, so a worker may still be inside the
    /// callback's `send` when the test's `recv` returns (the receiver unblocks
    /// as soon as the message is queued, before `send` finishes). Reclaiming the
    /// box here would free the `Sender` out from under that in-flight `send`, a
    /// use-after-free. The test deliberately leaks the context instead, which
    /// keeps the channel alive for the brief remaining life of any in-flight
    /// callback; the OS reclaims the memory at process exit.
    fn leak_ctx<T>(ptr: *mut T) {
        std::mem::forget(unsafe { Box::from_raw(ptr) });
    }

    #[test]
    fn run_task_calls_callback() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let name = CString::new("test-task").unwrap();

        weaveffi_tasks_run_task_async(name.as_ptr(), task_callback, tx_ptr as *mut c_void);

        let (code, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert_eq!(code, 0);

        let r = result.expect("success path passes a result buffer");
        assert!(r.id > 0);
        assert!(r.success);
        assert!(r.value.contains("test-task"));
    }

    #[test]
    fn run_task_empty_name_reports_invalid_name() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        let empty = CString::new("").unwrap();
        weaveffi_tasks_run_task_async(empty.as_ptr(), task_callback, tx_ptr as *mut c_void);

        let (code, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert_eq!(code, 1, "TaskError::InvalidName's declared code");
        assert!(result.is_none(), "error path passes a null result buffer");
    }

    #[test]
    fn run_task_null_name_reports_marshal_error_through_the_callback() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        // The launcher has no out_err slot, so an argument that fails to lift
        // is reported through the completion callback with the reserved
        // marshalling code, exactly like the sync path would report it.
        weaveffi_tasks_run_task_async(std::ptr::null(), task_callback, tx_ptr as *mut c_void);

        let (code, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert_eq!(code, abi::MARSHAL_ERROR_CODE);
        assert!(result.is_none());
    }

    #[test]
    fn run_batch_processes_sequentially() {
        let (tx, rx) = mpsc::channel::<BatchCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        // The list-of-strings parameter is buffered: encode `Vec<String>`
        // and pass the (ptr, len) pair. The launcher copies the bytes before
        // returning, so the local buffer only needs to outlive the call.
        let names = abi::encode_value(&vec![
            "task-a".to_string(),
            "task-b".to_string(),
            "task-c".to_string(),
        ]);

        weaveffi_tasks_run_batch_async(
            names.as_ptr(),
            names.len(),
            batch_callback,
            tx_ptr as *mut c_void,
        );

        let (had_error, results) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        leak_ctx(tx_ptr);
        assert!(!had_error);
        assert_eq!(results.len(), 3);

        for r in &results {
            assert!(r.id > 0);
            assert!(r.success);
        }

        assert!(results[0].value.contains("task-a"));
        assert!(results[2].value.contains("task-c"));
    }

    #[test]
    fn run_batch_empty_names() {
        let (tx, rx) = mpsc::channel::<BatchCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        let names = abi::encode_value(&Vec::<String>::new());
        weaveffi_tasks_run_batch_async(
            names.as_ptr(),
            names.len(),
            batch_callback,
            tx_ptr as *mut c_void,
        );

        let (had_error, results) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert!(!had_error);
        assert!(results.is_empty());
    }

    #[test]
    fn cancel_task_returns_false() {
        let mut err = weaveffi_error::default();
        let cancelled = weaveffi_tasks_cancel_task(42, &mut err);
        assert_eq!(err.code, 0);
        assert!(!cancelled);
    }

    #[test]
    fn task_result_buffer_round_trip() {
        // `TaskResult` crosses the ABI as a value buffer; the macro
        // implements `BufferValue`, so every field round-trips.
        let result = TaskResult {
            id: 42,
            value: "hello".to_string(),
            success: true,
        };
        let bytes = abi::encode_value(&result);
        let back = abi::decode_value::<TaskResult>(&bytes).unwrap();
        assert_eq!(back.id, 42);
        assert_eq!(back.value, "hello");
        assert!(back.success);
    }

    #[test]
    fn task_result_round_trips_success_false() {
        let result = TaskResult {
            id: 1,
            value: "fail".to_string(),
            success: false,
        };
        let bytes = abi::encode_value(&result);
        let back = abi::decode_value::<TaskResult>(&bytes).unwrap();
        assert!(!back.success);
    }

    #[test]
    fn run_n_tasks_invokes_callback_with_n() {
        let (tx, rx) = mpsc::channel::<(bool, i32)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_tasks_run_n_tasks_async(7, n_tasks_callback, tx_ptr as *mut c_void);
        let (had_error, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        leak_ctx(tx_ptr);
        assert!(!had_error);
        assert_eq!(result, 7);
    }

    #[test]
    fn active_callbacks_returns_to_zero() {
        let mut err = weaveffi_error::default();
        let (tx, rx) = mpsc::channel::<(bool, i32)>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        for i in 0..16 {
            weaveffi_tasks_run_n_tasks_async(i, n_tasks_callback, tx_ptr as *mut c_void);
        }
        for _ in 0..16 {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        leak_ctx(tx_ptr);

        for _ in 0..50 {
            if weaveffi_tasks_active_callbacks(&mut err) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(weaveffi_tasks_active_callbacks(&mut err), 0);
    }
}
