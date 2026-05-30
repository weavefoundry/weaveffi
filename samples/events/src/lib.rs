//! Events sample cdylib demonstrating module-level callbacks, listeners, and an
//! iterator over the WeaveFFI C ABI.
//!
//! The exported symbols implement *exactly* the ABI the WeaveFFI generators
//! emit (see the generated `weaveffi.h`): a context-carrying callback typedef,
//! `register_*`/`unregister_*` returning a `uint64_t` subscription id, and an
//! opaque iterator with an `int32_t next(iter, out_item, out_err)` contract.
//! The conformance harness binds the *generated* wrappers against this library,
//! so the two must agree.

#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use weaveffi_abi::{self as abi, weaveffi_error};

/// `typedef void (*weaveffi_events_OnMessage_fn)(const char* message, void* context);`
pub type OnMessageCallback = extern "C" fn(*const c_char, *mut c_void);

struct Listener {
    callback: OnMessageCallback,
    /// The opaque `void* context`, stored as `usize` so the registry is `Send`.
    context: usize,
}

static MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LISTENERS: Mutex<Option<HashMap<u64, Listener>>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn with_listeners<R>(f: impl FnOnce(&mut HashMap<u64, Listener>) -> R) -> R {
    let mut guard = LISTENERS.lock().unwrap();
    f(guard.get_or_insert_with(HashMap::new))
}

/// `uint64_t weaveffi_events_register_message_listener(OnMessage_fn, void* context);`
#[no_mangle]
pub extern "C" fn weaveffi_events_register_message_listener(
    callback: OnMessageCallback,
    context: *mut c_void,
) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    with_listeners(|m| {
        m.insert(
            id,
            Listener {
                callback,
                context: context as usize,
            },
        )
    });
    id
}

/// `void weaveffi_events_unregister_message_listener(uint64_t id);`
#[no_mangle]
pub extern "C" fn weaveffi_events_unregister_message_listener(id: u64) {
    with_listeners(|m| m.remove(&id));
}

#[no_mangle]
pub extern "C" fn weaveffi_events_send_message(text: *const c_char, out_err: *mut weaveffi_error) {
    let text = match abi::c_ptr_to_string(text) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, 1, "text is null or invalid UTF-8");
            return;
        }
    };
    MESSAGES.lock().unwrap().push(text.clone());

    // Snapshot (callback, context) pairs so we don't hold the registry lock
    // across foreign calls (which could re-enter register/unregister).
    let targets: Vec<(OnMessageCallback, usize)> =
        with_listeners(|m| m.values().map(|l| (l.callback, l.context)).collect());
    if !targets.is_empty() {
        let c_str = abi::string_to_c_ptr(&text);
        for (cb, ctx) in targets {
            cb(c_str, ctx as *mut c_void);
        }
        abi::free_string(c_str);
    }
    abi::error_set_ok(out_err);
}

/// Opaque iterator handle exported as `weaveffi_events_GetMessagesIterator`.
pub struct GetMessagesIterator {
    messages: Vec<String>,
    index: usize,
}

#[no_mangle]
pub extern "C" fn weaveffi_events_get_messages(
    out_err: *mut weaveffi_error,
) -> *mut GetMessagesIterator {
    let messages = MESSAGES.lock().unwrap().clone();
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(GetMessagesIterator { messages, index: 0 }))
}

/// `int32_t weaveffi_events_GetMessagesIterator_next(iter, const char** out_item, out_err);`
///
/// Writes the next element into `*out_item` and returns `1`, or returns `0`
/// when the iterator is exhausted (leaving `*out_item` untouched). The caller
/// owns the written string and frees it with `weaveffi_free_string`.
#[no_mangle]
pub extern "C" fn weaveffi_events_GetMessagesIterator_next(
    iter: *mut GetMessagesIterator,
    out_item: *mut *const c_char,
    out_err: *mut weaveffi_error,
) -> i32 {
    if iter.is_null() {
        abi::error_set(out_err, 1, "iterator is null");
        return 0;
    }
    let iter = unsafe { &mut *iter };
    abi::error_set_ok(out_err);
    if iter.index < iter.messages.len() {
        let msg = &iter.messages[iter.index];
        iter.index += 1;
        if !out_item.is_null() {
            unsafe { *out_item = abi::string_to_c_ptr(msg) };
        }
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_events_GetMessagesIterator_destroy(iter: *mut GetMessagesIterator) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter)) };
    }
}

abi::export_runtime!();

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        MESSAGES.lock().unwrap().clear();
        with_listeners(|m| m.clear());
        guard
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    #[test]
    fn send_and_iterate_messages() {
        let _g = setup();
        let mut err = new_err();

        for t in ["hello", "world"] {
            let c = CString::new(t).unwrap();
            weaveffi_events_send_message(c.as_ptr(), &mut err);
            assert_eq!(err.code, 0);
        }

        let iter = weaveffi_events_get_messages(&mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            let has = weaveffi_events_GetMessagesIterator_next(iter, &mut item, &mut err);
            assert_eq!(err.code, 0);
            if has == 0 {
                break;
            }
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_events_GetMessagesIterator_destroy(iter);
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn get_messages_empty() {
        let _g = setup();
        let mut err = new_err();
        let iter = weaveffi_events_get_messages(&mut err);
        let mut item: *const c_char = std::ptr::null();
        let has = weaveffi_events_GetMessagesIterator_next(iter, &mut item, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(has, 0);
        weaveffi_events_GetMessagesIterator_destroy(iter);
    }

    #[test]
    fn listener_receives_messages_with_context() {
        let _g = setup();
        let mut err = new_err();

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn on_message(_msg: *const c_char, ctx: *mut c_void) {
            assert!(!ctx.is_null());
            let counter = unsafe { &*(ctx as *const AtomicUsize) };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        COUNT.store(0, Ordering::Relaxed);

        let id = weaveffi_events_register_message_listener(
            on_message,
            &COUNT as *const AtomicUsize as *mut c_void,
        );
        assert!(id > 0);

        let text = CString::new("test").unwrap();
        weaveffi_events_send_message(text.as_ptr(), &mut err);
        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 2);

        weaveffi_events_unregister_message_listener(id);
        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn destroy_null_iterator_is_safe() {
        weaveffi_events_GetMessagesIterator_destroy(std::ptr::null_mut());
    }

    #[test]
    fn next_null_iterator_returns_error() {
        let mut err = new_err();
        let mut item: *const c_char = std::ptr::null();
        let has =
            weaveffi_events_GetMessagesIterator_next(std::ptr::null_mut(), &mut item, &mut err);
        assert_eq!(has, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }
}
