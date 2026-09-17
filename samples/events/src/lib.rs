//! Events sample cdylib: a publish/subscribe bus built on a WeaveFFI
//! callback interface, a reference-counted object, and an iterator.
//!
//! The `#[weaveffi::module]` expansion emits exactly the ABI the WeaveFFI
//! generators bind to (see the generated `weaveffi.h`): a `Subscriber` vtable
//! the consumer implements, an `EventBus` object with `_clone`/`_destroy`
//! reference counting, and an opaque iterator with an
//! `int32_t next(iter, out_item, out_err)` contract. The conformance harness
//! binds the generated wrappers of every language against this library, so the
//! two must agree.
//!
//! The producer writes only safe Rust. The consumer's subscriber arrives as an
//! `Arc<dyn Subscriber>`; the bus retains it for as long as it likes and the
//! consumer's `free` entry fires when the last reference drops. A subscriber
//! that fails aborts the publishing call with `FOREIGN_ERROR_CODE`, so the bus
//! snapshots its subscriber list before calling out and never holds a lock
//! across a callback.

/// A publish/subscribe event bus driven by a consumer-implemented subscriber.
#[weaveffi::module]
pub mod events {
    use std::sync::{Arc, Mutex, PoisonError};

    /// How a subscriber wants to be told about a message.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Delivery {
        /// Deliver the message.
        Accept = 0,
        /// Skip this subscriber for this message.
        Skip = 1,
        /// Deliver the message and stop delivering to later subscribers.
        AcceptAndStop = 2,
    }

    /// A published message as subscribers see it.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Message {
        /// Monotonic sequence number, starting at 1.
        pub seq: i64,
        /// Topic the message was published under.
        pub topic: String,
        /// Message text.
        pub text: String,
        /// Free-form labels attached at publish time.
        pub tags: Vec<String>,
    }

    /// A consumer-implemented subscriber. The bus asks `route` whether to
    /// deliver each message and then calls `on_message` for accepted ones.
    #[weaveffi::callback_interface]
    pub trait Subscriber: Send + Sync {
        /// Decide how the bus should treat `topic` for this subscriber.
        fn route(&self, topic: String) -> Delivery;
        /// Receive an accepted message. Returns the subscriber's running count
        /// of received messages.
        fn on_message(&self, message: &Message) -> i64;
        /// Receive the bus itself (an object handed through a callback). The
        /// consumer adopts the reference and may keep or drop it.
        fn on_attached(&self, bus: Arc<EventBus>);
    }

    /// A bus that retains its subscribers and logs every message.
    #[weaveffi::interface]
    pub struct EventBus {
        subscribers: Mutex<Vec<Arc<dyn Subscriber>>>,
        log: Mutex<Vec<Message>>,
    }

    impl EventBus {
        /// Create an empty bus.
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                subscribers: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
            })
        }

        /// Retain `subscriber` and tell it which bus it joined. Returns the
        /// new subscriber count.
        pub fn subscribe(self: Arc<Self>, subscriber: Arc<dyn Subscriber>) -> i64 {
            subscriber.on_attached(Arc::clone(&self));
            let mut subs = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            subs.push(subscriber);
            subs.len() as i64
        }

        /// Publish `text` under `topic`, returning how many subscribers
        /// accepted it. A subscriber failure aborts the call.
        pub fn publish(&self, topic: String, text: String, tags: Vec<String>) -> i64 {
            let message = {
                let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
                let message = Message {
                    seq: log.len() as i64 + 1,
                    topic,
                    text,
                    tags,
                };
                log.push(message.clone());
                message
            };
            // Snapshot so no lock is held while the consumer runs.
            let subs: Vec<Arc<dyn Subscriber>> = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let mut delivered = 0;
            for sub in &subs {
                match sub.route(message.topic.clone()) {
                    Delivery::Skip => {}
                    Delivery::Accept => {
                        sub.on_message(&message);
                        delivered += 1;
                    }
                    Delivery::AcceptAndStop => {
                        sub.on_message(&message);
                        delivered += 1;
                        break;
                    }
                }
            }
            delivered
        }

        /// Publish from a producer thread, resolving with the accepted count.
        pub async fn publish_later(&self, topic: String, text: String) -> i64 {
            self.publish(topic, text, Vec::new())
        }

        /// Number of retained subscribers.
        pub fn subscriber_count(&self) -> i64 {
            self.subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len() as i64
        }

        /// Drop every subscriber; each consumer `free` entry runs when its
        /// last reference goes away.
        pub fn clear_subscribers(&self) {
            self.subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }

        /// Stream the text of every message published so far, in order.
        pub fn messages(&self) -> weaveffi::Iter<String> {
            let texts: Vec<String> = self
                .log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|m| m.text.clone())
                .collect();
            weaveffi::Iter::new(texts)
        }

        /// The most recent message, if any.
        pub fn last_message(&self) -> Option<Message> {
            self.log
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last()
                .cloned()
        }
    }

    /// Ask `subscriber` how it would route `topic` without a bus.
    #[weaveffi::export]
    pub fn route_once(subscriber: Arc<dyn Subscriber>, topic: String) -> Delivery {
        subscriber.route(topic)
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::events::*;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi::abi::{self, weaveffi_error};

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    /// A consumer-side subscriber, exactly as a generated binding builds one:
    /// a heap-allocated context and a process-wide vtable.
    struct SubState {
        received: AtomicI64,
        attached: AtomicUsize,
        skip_topic: String,
        fail_topic: String,
        freed: Arc<AtomicUsize>,
    }

    unsafe extern "C" fn route(
        ctx: *mut c_void,
        topic: *const c_char,
        out_err: *mut weaveffi_error,
    ) -> i32 {
        let state = &*(ctx as *const SubState);
        let topic = abi::c_ptr_to_string(topic).unwrap();
        if topic == state.fail_topic {
            abi::error_set(
                out_err,
                abi::FOREIGN_ERROR_CODE,
                "subscriber rejected topic",
            );
            return 0;
        }
        if topic == state.skip_topic {
            Delivery::Skip as i32
        } else if topic == "stop" {
            Delivery::AcceptAndStop as i32
        } else {
            Delivery::Accept as i32
        }
    }

    unsafe extern "C" fn on_message(
        ctx: *mut c_void,
        message_ptr: *const u8,
        message_len: usize,
        _out_err: *mut weaveffi_error,
    ) -> i64 {
        let state = &*(ctx as *const SubState);
        let message: Message =
            abi::decode_value(std::slice::from_raw_parts(message_ptr, message_len)).unwrap();
        assert!(message.seq >= 1);
        state.received.fetch_add(1, Ordering::SeqCst) + 1
    }

    unsafe extern "C" fn on_attached(
        ctx: *mut c_void,
        bus: *mut EventBus,
        _out_err: *mut weaveffi_error,
    ) {
        let state = &*(ctx as *const SubState);
        state.attached.fetch_add(1, Ordering::SeqCst);
        // The reference is ours: dropping it must not free the live bus.
        weaveffi_events_EventBus_destroy(bus);
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        let state = Box::from_raw(ctx as *mut SubState);
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    static VTABLE: weaveffi_events_Subscriber_vtable = weaveffi_events_Subscriber_vtable {
        route,
        on_message,
        on_attached,
        free,
    };

    fn new_sub(skip: &str, fail: &str, freed: &Arc<AtomicUsize>) -> *mut c_void {
        Box::into_raw(Box::new(SubState {
            received: AtomicI64::new(0),
            attached: AtomicUsize::new(0),
            skip_topic: skip.to_string(),
            fail_topic: fail.to_string(),
            freed: Arc::clone(freed),
        })) as *mut c_void
    }

    fn publish(bus: *mut EventBus, topic: &str, text: &str, err: &mut weaveffi_error) -> i64 {
        let topic = CString::new(topic).unwrap();
        let text = CString::new(text).unwrap();
        let tags = abi::encode_value(&vec!["a".to_string()]);
        weaveffi_events_EventBus_publish(
            bus,
            topic.as_ptr(),
            text.as_ptr(),
            tags.as_ptr(),
            tags.len(),
            err,
        )
    }

    #[test]
    fn subscribe_publish_and_iterate() {
        let mut err = new_err();
        let freed = Arc::new(AtomicUsize::new(0));
        let bus = weaveffi_events_EventBus_new(&mut err);
        assert!(!bus.is_null());

        let a = new_sub("quiet", "", &freed);
        let b = new_sub("", "", &freed);
        assert_eq!(
            weaveffi_events_EventBus_subscribe(bus, a, &VTABLE, &mut err),
            1
        );
        assert_eq!(
            weaveffi_events_EventBus_subscribe(bus, b, &VTABLE, &mut err),
            2
        );
        let a_state = unsafe { &*(a as *const SubState) };
        assert_eq!(a_state.attached.load(Ordering::SeqCst), 1);

        assert_eq!(publish(bus, "news", "hello", &mut err), 2);
        assert_eq!(publish(bus, "quiet", "psst", &mut err), 1);
        assert_eq!(publish(bus, "stop", "last", &mut err), 1);
        assert_eq!(err.code, 0);

        let iter = weaveffi_events_EventBus_messages(bus, &mut err);
        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            if weaveffi_events_EventBus_MessagesIterator_next(iter, &mut item, &mut err) == 0 {
                break;
            }
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_events_EventBus_MessagesIterator_destroy(iter);
        assert_eq!(got, vec!["hello", "psst", "last"]);

        let mut len = 0usize;
        let ptr = weaveffi_events_EventBus_last_message(bus, &mut len, &mut err);
        let last: Option<Message> =
            abi::decode_value(unsafe { std::slice::from_raw_parts(ptr, len) }).unwrap();
        abi::free_bytes(ptr as *mut u8, len);
        assert_eq!(last.unwrap().text, "last");

        weaveffi_events_EventBus_clear_subscribers(bus, &mut err);
        assert_eq!(
            freed.load(Ordering::SeqCst),
            2,
            "free ran once per subscriber"
        );
        weaveffi_events_EventBus_destroy(bus);
    }

    #[test]
    fn foreign_error_aborts_publish() {
        let mut err = new_err();
        let freed = Arc::new(AtomicUsize::new(0));
        let bus = weaveffi_events_EventBus_new(&mut err);
        let a = new_sub("", "boom", &freed);
        weaveffi_events_EventBus_subscribe(bus, a, &VTABLE, &mut err);
        publish(bus, "boom", "x", &mut err);
        assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
        assert!(abi::c_ptr_to_string(err.message)
            .unwrap()
            .contains("rejected topic"));
        abi::error_clear(&mut err);

        // The bus is still usable afterward.
        assert_eq!(publish(bus, "ok", "y", &mut err), 1);
        assert_eq!(err.code, 0);
        weaveffi_events_EventBus_destroy(bus);
        assert_eq!(
            freed.load(Ordering::SeqCst),
            1,
            "destroying the bus frees its subscriber"
        );
    }

    #[test]
    fn route_once_does_not_retain() {
        let mut err = new_err();
        let freed = Arc::new(AtomicUsize::new(0));
        let a = new_sub("quiet", "", &freed);
        let topic = CString::new("quiet").unwrap();
        let d = weaveffi_events_route_once(a, &VTABLE, topic.as_ptr(), &mut err);
        assert_eq!(d, Delivery::Skip as i32);
        assert_eq!(freed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reference_counting() {
        let mut err = new_err();
        let bus = weaveffi_events_EventBus_new(&mut err);
        let again = weaveffi_events_EventBus_clone(bus);
        assert_eq!(bus, again);
        weaveffi_events_EventBus_destroy(bus);
        assert_eq!(
            weaveffi_events_EventBus_subscriber_count(again, &mut err),
            0
        );
        weaveffi_events_EventBus_destroy(again);
        weaveffi_events_EventBus_destroy(std::ptr::null_mut());
    }
}
