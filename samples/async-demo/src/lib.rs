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

    /// Run a single named task, completing with its `TaskResult`.
    #[weaveffi::export]
    pub async fn run_task(name: String) -> TaskResult {
        let _guard = ActiveGuard::new();
        TaskResult {
            id: next_id(),
            value: format!("completed: {name}"),
            success: true,
        }
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

    type TaskCbMsg = (bool, *mut TaskResult);
    type BatchCbMsg = (bool, *mut *mut TaskResult, usize);

    extern "C" fn task_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        result: *mut TaskResult,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<TaskCbMsg>) };
        let had_error = !err.is_null() && unsafe { (*err).code } != 0;
        tx.send((had_error, result)).unwrap();
    }

    extern "C" fn batch_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        results: *mut *mut TaskResult,
        results_len: usize,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<BatchCbMsg>) };
        let had_error = !err.is_null() && unsafe { (*err).code } != 0;
        tx.send((had_error, results, results_len)).unwrap();
    }

    extern "C" fn n_tasks_callback(context: *mut c_void, err: *mut weaveffi_error, result: i32) {
        let tx = unsafe { &*(context as *const mpsc::Sender<(bool, i32)>) };
        let had_error = !err.is_null() && unsafe { (*err).code } != 0;
        tx.send((had_error, result)).unwrap();
    }

    /// Free a returned `[TaskResult]`: destroy each element, then reclaim the
    /// pointer array the launcher allocated (the same shape the conformance
    /// consumers free a list-of-struct return).
    fn free_results(results: *mut *mut TaskResult, len: usize) {
        if results.is_null() {
            return;
        }
        for i in 0..len {
            weaveffi_tasks_TaskResult_destroy(unsafe { *results.add(i) });
        }
        unsafe { drop(Vec::from_raw_parts(results, len, len)) };
    }

    #[test]
    fn run_task_calls_callback() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let name = CString::new("test-task").unwrap();

        weaveffi_tasks_run_task_async(name.as_ptr(), task_callback, tx_ptr as *mut c_void);

        let (had_error, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert!(!had_error);
        assert!(!result.is_null());

        let r = unsafe { &*result };
        assert!(r.id > 0);
        assert!(r.success);
        assert!(r.value.contains("test-task"));

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn run_task_null_name() {
        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        weaveffi_tasks_run_task_async(std::ptr::null(), task_callback, tx_ptr as *mut c_void);

        let (had_error, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert!(!had_error);
        assert!(!result.is_null());

        let r = unsafe { &*result };
        assert!(r.success);

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn run_batch_processes_sequentially() {
        let (tx, rx) = mpsc::channel::<BatchCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let names: Vec<CString> = vec![
            CString::new("task-a").unwrap(),
            CString::new("task-b").unwrap(),
            CString::new("task-c").unwrap(),
        ];
        let name_ptrs: Vec<*const std::os::raw::c_char> =
            names.iter().map(|n| n.as_ptr()).collect();

        weaveffi_tasks_run_batch_async(
            name_ptrs.as_ptr(),
            name_ptrs.len(),
            batch_callback,
            tx_ptr as *mut c_void,
        );

        let (had_error, results, results_len) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert!(!had_error);
        assert_eq!(results_len, 3);
        assert!(!results.is_null());

        for i in 0..results_len {
            let r = unsafe { &**results.add(i) };
            assert!(r.id > 0);
            assert!(r.success);
        }

        let r0 = unsafe { &**results };
        assert!(r0.value.contains("task-a"));
        let r2 = unsafe { &**results.add(2) };
        assert!(r2.value.contains("task-c"));

        free_results(results, results_len);
    }

    #[test]
    fn run_batch_empty_names() {
        let (tx, rx) = mpsc::channel::<BatchCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        weaveffi_tasks_run_batch_async(std::ptr::null(), 0, batch_callback, tx_ptr as *mut c_void);

        let (had_error, results, results_len) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert!(!had_error);
        assert_eq!(results_len, 0);
        assert!(results.is_null());
    }

    #[test]
    fn cancel_task_returns_false() {
        let mut err = weaveffi_error::default();
        let cancelled = weaveffi_tasks_cancel_task(42, &mut err);
        assert_eq!(err.code, 0);
        assert!(!cancelled);
    }

    #[test]
    fn task_result_getters() {
        let mut err = weaveffi_error::default();
        let value = CString::new("hello").unwrap();
        let result = weaveffi_tasks_TaskResult_create(42, value.as_ptr(), true, &mut err);
        assert_eq!(err.code, 0);
        assert!(!result.is_null());

        assert_eq!(weaveffi_tasks_TaskResult_get_id(result), 42);
        assert!(weaveffi_tasks_TaskResult_get_success(result));

        let got = weaveffi_tasks_TaskResult_get_value(result);
        assert_eq!(abi::c_ptr_to_string(got).unwrap(), "hello");
        abi::free_string(got);

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn task_result_success_false() {
        let mut err = weaveffi_error::default();
        let value = CString::new("fail").unwrap();
        let result = weaveffi_tasks_TaskResult_create(1, value.as_ptr(), false, &mut err);
        assert!(!weaveffi_tasks_TaskResult_get_success(result));
        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn destroy_null_task_result_is_safe() {
        weaveffi_tasks_TaskResult_destroy(std::ptr::null_mut());
    }

    #[test]
    fn run_n_tasks_invokes_callback_with_n() {
        let (tx, rx) = mpsc::channel::<(bool, i32)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_tasks_run_n_tasks_async(7, n_tasks_callback, tx_ptr as *mut c_void);
        let (had_error, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
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
        unsafe { drop(Box::from_raw(tx_ptr)) };

        for _ in 0..50 {
            if weaveffi_tasks_active_callbacks(&mut err) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(weaveffi_tasks_active_callbacks(&mut err), 0);
    }
}
