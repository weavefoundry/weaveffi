//! An async launcher called with a null callback discards the result: the
//! returned run or object is never lowered, so nothing outlives the call.
//! This runs in its own test binary because it installs a process-wide
//! spawner (one that keeps every task's thread so the test can join them)
//! and reads the process-wide leak counters.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use weaveffi::__rt::{debug_live, leak, FfiError, Run};

weaveffi::export_runtime!();

/// A returned object.
#[weaveffi::interface]
pub struct Slot {
    /// The slot number.
    pub n: i32,
}

/// Describe a slot later (an async run result).
#[weaveffi::export]
pub async fn describe_later(n: i32) -> String {
    format!("slot {n}")
}

/// Make a slot later (an async object result).
#[weaveffi::export]
pub async fn slot_later(n: i32) -> Arc<Slot> {
    Arc::new(Slot { n })
}

unsafe extern "C" {
    fn null_callback_describe_later(
        n: i32,
        callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError, Run)>,
        context: *mut c_void,
    );
    fn null_callback_slot_later(
        n: i32,
        callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError, *const Slot)>,
        context: *mut c_void,
    );
}

static TASKS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

#[test]
fn a_null_callback_releases_the_result() {
    const { assert!(leak::ENABLED) };
    weaveffi::set_spawner(|fut: weaveffi::BoxFuture| {
        let task = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(fut);
        });
        TASKS.lock().unwrap().push(task);
    })
    .unwrap();

    unsafe {
        null_callback_describe_later(1, None, std::ptr::null_mut());
        null_callback_slot_later(2, None, std::ptr::null_mut());
    }
    // Each task lowers (or drops) its result before its thread ends.
    let tasks = std::mem::take(&mut *TASKS.lock().unwrap());
    assert_eq!(tasks.len(), 2);
    for task in tasks {
        task.join().unwrap();
    }
    assert_eq!(debug_live(leak::OBJECTS), 0, "the object leaked");
    assert_eq!(debug_live(leak::ALLOCATIONS), 0, "the string run leaked");
}
