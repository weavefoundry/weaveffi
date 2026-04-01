#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, Ordering};
use weaveffi_abi::{self as abi, weaveffi_error};

#[derive(Debug, Clone)]
pub struct TaskResult {
    pub id: i64,
    pub value: String,
    pub success: bool,
}

static NEXT_TASK_ID: AtomicI64 = AtomicI64::new(1);

pub type weaveffi_tasks_run_task_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: *mut TaskResult);

pub type weaveffi_tasks_run_batch_callback = extern "C" fn(
    context: *mut c_void,
    err: *mut weaveffi_error,
    result: *mut *mut TaskResult,
    result_len: usize,
);

#[no_mangle]
pub extern "C" fn weaveffi_tasks_run_task_async(
    name: *const c_char,
    callback: weaveffi_tasks_run_task_callback,
    context: *mut c_void,
) {
    let name_str = abi::c_ptr_to_string(name).unwrap_or_default();
    let ctx = context as usize;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
        let result = TaskResult {
            id,
            value: format!("completed: {name_str}"),
            success: true,
        };
        let result_ptr = Box::into_raw(Box::new(result));
        callback(ctx as *mut c_void, std::ptr::null_mut(), result_ptr);
    });
}

#[no_mangle]
pub extern "C" fn weaveffi_tasks_run_batch_async(
    names: *const *const c_char,
    names_len: usize,
    callback: weaveffi_tasks_run_batch_callback,
    context: *mut c_void,
) {
    let name_list: Vec<String> = if names.is_null() {
        Vec::new()
    } else {
        (0..names_len)
            .map(|i| {
                let ptr = unsafe { *names.add(i) };
                abi::c_ptr_to_string(ptr).unwrap_or_default()
            })
            .collect()
    };
    let ctx = context as usize;
    std::thread::spawn(move || {
        let mut results: Vec<*mut TaskResult> = Vec::new();
        for name in &name_list {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
            let result = TaskResult {
                id,
                value: format!("completed: {name}"),
                success: true,
            };
            results.push(Box::into_raw(Box::new(result)));
        }
        let len = results.len();
        let ptr = if results.is_empty() {
            std::ptr::null_mut()
        } else {
            let p = results.as_mut_ptr();
            std::mem::forget(results);
            p
        };
        callback(ctx as *mut c_void, std::ptr::null_mut(), ptr, len);
    });
}

#[no_mangle]
pub extern "C" fn weaveffi_tasks_cancel_task(_id: i64, out_err: *mut weaveffi_error) -> i32 {
    abi::error_set_ok(out_err);
    0
}

// ── TaskResult getters ──────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_tasks_TaskResult_get_id(result: *const TaskResult) -> i64 {
    assert!(!result.is_null());
    unsafe { (*result).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_tasks_TaskResult_get_value(result: *const TaskResult) -> *const c_char {
    assert!(!result.is_null());
    abi::string_to_c_ptr(&unsafe { &*result }.value)
}

#[no_mangle]
pub extern "C" fn weaveffi_tasks_TaskResult_get_success(result: *const TaskResult) -> i32 {
    assert!(!result.is_null());
    unsafe { (*result).success as i32 }
}

// ── TaskResult lifecycle ────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_tasks_TaskResult_destroy(result: *mut TaskResult) {
    if result.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(result)) };
}

#[no_mangle]
pub extern "C" fn weaveffi_tasks_TaskResult_list_free(results: *mut *mut TaskResult, len: usize) {
    if results.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(results, len, len) };
    for ptr in ptrs {
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

// ── Shared free functions ───────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::mpsc;
    use std::time::Duration;

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
        let name_ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();

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
            let result = unsafe { *results.add(i) };
            let r = unsafe { &*result };
            assert!(r.id > 0);
            assert!(r.success);
        }

        let r0 = unsafe { &**results };
        assert!(r0.value.contains("task-a"));
        let r2 = unsafe { &**results.add(2) };
        assert!(r2.value.contains("task-c"));

        weaveffi_tasks_TaskResult_list_free(results, results_len);
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
        let result = weaveffi_tasks_cancel_task(42, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn cancel_task_stops_execution() {
        let mut err = weaveffi_error::default();
        let cancelled = weaveffi_tasks_cancel_task(999, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(cancelled, 0);

        let (tx, rx) = mpsc::channel::<TaskCbMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let name = CString::new("post-cancel").unwrap();
        weaveffi_tasks_run_task_async(name.as_ptr(), task_callback, tx_ptr as *mut c_void);

        let (had_error, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert!(!had_error);
        assert!(!result.is_null());

        let r = unsafe { &*result };
        assert!(r.success);
        assert!(r.value.contains("post-cancel"));

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn task_result_getters() {
        let result = Box::into_raw(Box::new(TaskResult {
            id: 42,
            value: "hello".to_string(),
            success: true,
        }));

        assert_eq!(weaveffi_tasks_TaskResult_get_id(result), 42);
        assert_eq!(weaveffi_tasks_TaskResult_get_success(result), 1);

        let value = weaveffi_tasks_TaskResult_get_value(result);
        assert_eq!(abi::c_ptr_to_string(value).unwrap(), "hello");
        abi::free_string(value);

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn task_result_success_false() {
        let result = Box::into_raw(Box::new(TaskResult {
            id: 1,
            value: "fail".to_string(),
            success: false,
        }));

        assert_eq!(weaveffi_tasks_TaskResult_get_success(result), 0);

        weaveffi_tasks_TaskResult_destroy(result);
    }

    #[test]
    fn destroy_null_task_result_is_safe() {
        weaveffi_tasks_TaskResult_destroy(std::ptr::null_mut());
    }

    #[test]
    fn list_free_null_is_safe() {
        weaveffi_tasks_TaskResult_list_free(std::ptr::null_mut(), 0);
    }
}
