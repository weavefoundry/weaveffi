#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use std::sync::Mutex;
use weaveffi_abi::{self as abi, weaveffi_error};

static MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LISTENER: Mutex<Option<extern "C" fn(*const c_char)>> = Mutex::new(None);

pub type OnMessageCallback = extern "C" fn(*const c_char);

pub struct MessageIterator {
    messages: Vec<String>,
    index: usize,
}

#[no_mangle]
pub extern "C" fn weaveffi_events_message_listener_register(
    callback: OnMessageCallback,
    out_err: *mut weaveffi_error,
) {
    *LISTENER.lock().unwrap() = Some(callback);
    abi::error_set_ok(out_err);
}

#[no_mangle]
pub extern "C" fn weaveffi_events_message_listener_unregister(out_err: *mut weaveffi_error) {
    *LISTENER.lock().unwrap() = None;
    abi::error_set_ok(out_err);
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
    let cb = *LISTENER.lock().unwrap();
    if let Some(cb) = cb {
        let c_str = abi::string_to_c_ptr(&text);
        cb(c_str);
        abi::free_string(c_str);
    }
    abi::error_set_ok(out_err);
}

#[no_mangle]
pub extern "C" fn weaveffi_events_get_messages(
    out_err: *mut weaveffi_error,
) -> *mut MessageIterator {
    let messages = MESSAGES.lock().unwrap().clone();
    let iter = MessageIterator { messages, index: 0 };
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(iter))
}

#[no_mangle]
pub extern "C" fn weaveffi_events_MessageIterator_next(
    iter: *mut MessageIterator,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    if iter.is_null() {
        abi::error_set(out_err, 1, "iterator is null");
        return std::ptr::null();
    }
    let iter = unsafe { &mut *iter };
    if iter.index < iter.messages.len() {
        let msg = &iter.messages[iter.index];
        iter.index += 1;
        abi::error_set_ok(out_err);
        abi::string_to_c_ptr(msg)
    } else {
        abi::error_set_ok(out_err);
        std::ptr::null()
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_events_MessageIterator_destroy(iter: *mut MessageIterator) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter)) };
    }
}

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
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        MESSAGES.lock().unwrap().clear();
        *LISTENER.lock().unwrap() = None;
        guard
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    #[test]
    fn send_and_iterate_messages() {
        let _g = setup();
        let mut err = new_err();

        let t1 = CString::new("hello").unwrap();
        weaveffi_events_send_message(t1.as_ptr(), &mut err);
        assert_eq!(err.code, 0);

        let t2 = CString::new("world").unwrap();
        weaveffi_events_send_message(t2.as_ptr(), &mut err);
        assert_eq!(err.code, 0);

        let iter = weaveffi_events_get_messages(&mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let msg1 = weaveffi_events_MessageIterator_next(iter, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(msg1).unwrap(), "hello");
        abi::free_string(msg1);

        let msg2 = weaveffi_events_MessageIterator_next(iter, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(msg2).unwrap(), "world");
        abi::free_string(msg2);

        let msg3 = weaveffi_events_MessageIterator_next(iter, &mut err);
        assert_eq!(err.code, 0);
        assert!(msg3.is_null());

        weaveffi_events_MessageIterator_destroy(iter);
    }

    #[test]
    fn send_message_null_text() {
        let _g = setup();
        let mut err = new_err();
        weaveffi_events_send_message(std::ptr::null(), &mut err);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn get_messages_empty() {
        let _g = setup();
        let mut err = new_err();

        let iter = weaveffi_events_get_messages(&mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let msg = weaveffi_events_MessageIterator_next(iter, &mut err);
        assert_eq!(err.code, 0);
        assert!(msg.is_null());

        weaveffi_events_MessageIterator_destroy(iter);
    }

    #[test]
    fn listener_receives_messages() {
        let _g = setup();
        let mut err = new_err();

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn on_message(_msg: *const c_char) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        COUNT.store(0, Ordering::Relaxed);

        weaveffi_events_message_listener_register(on_message, &mut err);
        assert_eq!(err.code, 0);

        let text = CString::new("test").unwrap();
        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn listener_unregister_stops_callbacks() {
        let _g = setup();
        let mut err = new_err();

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn on_message(_msg: *const c_char) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        COUNT.store(0, Ordering::Relaxed);

        weaveffi_events_message_listener_register(on_message, &mut err);
        let text = CString::new("a").unwrap();
        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_events_message_listener_unregister(&mut err);
        assert_eq!(err.code, 0);

        weaveffi_events_send_message(text.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn destroy_null_iterator_is_safe() {
        weaveffi_events_MessageIterator_destroy(std::ptr::null_mut());
    }

    #[test]
    fn next_null_iterator_returns_error() {
        let mut err = new_err();
        let msg = weaveffi_events_MessageIterator_next(std::ptr::null_mut(), &mut err);
        assert!(msg.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }
}
