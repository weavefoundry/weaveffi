//! Events sample cdylib demonstrating module-level callbacks, listeners, and an
//! iterator over the WeaveFFI C ABI.
//!
//! The `#[weaveffi::module]` expansion emits *exactly* the ABI the WeaveFFI
//! generators emit (see the generated `weaveffi.h`): a context-carrying
//! callback typedef, `register_*`/`unregister_*` returning a `uint64_t`
//! subscription id, and an opaque iterator with an
//! `int32_t next(iter, out_item, out_err)` contract. The conformance harness
//! binds the *generated* wrappers against this library, so the two must agree.
//!
//! The producer writes only safe Rust: it declares the callback and listener
//! with marker attributes, keeps a message log, and calls the generated
//! `emit_message_listener` helper to fan a message out to every subscriber.

/// Callback and listener-based event subscription example.
#[weaveffi::module]
pub mod events {
    use std::sync::Mutex;

    /// Every message ever sent, replayed by `get_messages`. `pub(crate)` so the
    /// in-crate unit tests can reset it between runs; it is not a tagged item,
    /// so it never appears in the generated bindings.
    pub(crate) static MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Fires once per sent message with the message text.
    #[weaveffi::callback]
    #[allow(non_snake_case, dead_code, unused_variables)]
    fn OnMessage(message: String) {}

    /// The set of subscribers notified on each `send_message`.
    #[weaveffi::listener(event = "OnMessage")]
    #[allow(dead_code)]
    fn message_listener() {}

    /// Send a message, triggering the OnMessage callback
    #[weaveffi::export]
    pub fn send_message(text: String) {
        MESSAGES.lock().unwrap().push(text.clone());
        emit_message_listener(&text);
    }

    /// Return an iterator over all sent messages
    #[weaveffi::export]
    pub fn get_messages() -> weaveffi::Iter<String> {
        let messages = MESSAGES.lock().unwrap().clone();
        weaveffi::Iter::new(messages)
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::events::*;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use weaveffi::abi::{self, weaveffi_error};

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        crate::events::MESSAGES.lock().unwrap().clear();
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
