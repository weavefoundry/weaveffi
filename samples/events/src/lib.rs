//! Events sample cdylib: a publish/subscribe bus built on a WeaveFFI
//! callback interface, a reference-counted object, and an iterator.
//!
//! The `#[weaveffi::module]` expansion emits exactly the ABI the WeaveFFI
//! generators bind to (see the generated `events.h`): a `Subscriber` vtable
//! the consumer implements, an `EventBus` object with `_clone`/`_destroy`
//! reference counting, and an opaque iterator with an
//! `int32_t next(iter, out_item, out_err)` contract. The conformance harness
//! binds the generated wrappers of every language against this library, so the
//! two must agree.
//!
//! The producer writes only safe Rust. The consumer's subscriber arrives as an
//! `Arc<dyn Subscriber>`; the bus retains it for as long as it likes and the
//! consumer's `free` entry fires when the last reference drops.
//!
//! The two callback styles are both on show. `route` returns
//! `Result<Delivery, ForeignError>`, so a consumer failure comes back as a
//! value; the bus chooses to propagate it, aborting the call with
//! `FOREIGN_ERROR_CODE`. `on_message` and `on_attached` return plain values,
//! so a consumer failure there aborts the call directly. Either way the bus
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
        /// Decide how the bus should treat `topic` for this subscriber. A
        /// consumer failure is returned as an `Err`.
        fn route(&self, topic: &str) -> Result<Delivery, weaveffi::ForeignError>;
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
                let delivery = match sub.route(&message.topic) {
                    Ok(d) => d,
                    Err(e) => {
                        // Propagate the subscriber's failure to our caller,
                        // exactly as a plain-return method would have.
                        weaveffi::abi::raise_foreign_error(e);
                        return delivered;
                    }
                };
                match delivery {
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

    /// Ask `subscriber` how it would route `topic` without a bus. A
    /// subscriber failure fails the call.
    #[weaveffi::export]
    pub fn route_once(subscriber: Arc<dyn Subscriber>, topic: &str) -> Delivery {
        subscriber.route(topic).unwrap_or_else(|e| {
            weaveffi::abi::raise_foreign_error(e);
            Delivery::Skip
        })
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::events::*;
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi::abi::{self, FfiError};

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
        topic_ptr: *const u8,
        topic_len: usize,
        out_err: *mut FfiError,
    ) -> i32 {
        let state = unsafe { &*(ctx as *const SubState) };
        // String arguments are borrowed `(ptr, len)` runs for the call.
        let topic = unsafe { abi::lift_str(topic_ptr, topic_len) }.unwrap();
        if topic == state.fail_topic {
            let msg = c"subscriber rejected topic";
            unsafe { crate::events_error_set(out_err, abi::FOREIGN_ERROR_CODE, msg.as_ptr()) };
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
        _out_err: *mut FfiError,
    ) -> i64 {
        let state = unsafe { &*(ctx as *const SubState) };
        let message: Message =
            abi::decode_value(unsafe { std::slice::from_raw_parts(message_ptr, message_len) })
                .unwrap();
        assert!(message.seq >= 1);
        state.received.fetch_add(1, Ordering::SeqCst) + 1
    }

    unsafe extern "C" fn on_attached(ctx: *mut c_void, bus: *mut EventBus, _err: *mut FfiError) {
        let state = unsafe { &*(ctx as *const SubState) };
        state.attached.fetch_add(1, Ordering::SeqCst);
        // The reference is ours: dropping it must not free the live bus.
        unsafe { events_events_EventBus_destroy(bus) };
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        let state = unsafe { Box::from_raw(ctx as *mut SubState) };
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    static VTABLE: events_events_Subscriber_vtable = events_events_Subscriber_vtable {
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

    fn new_bus() -> *mut EventBus {
        let mut err = FfiError::default();
        let bus = unsafe { events_events_EventBus_new(&mut err) };
        assert!(!bus.is_null());
        bus
    }

    fn subscribe(bus: *mut EventBus, sub: *mut c_void) -> i64 {
        let mut err = FfiError::default();
        unsafe { events_events_EventBus_subscribe(bus, sub, &VTABLE, &mut err) }
    }

    fn publish(bus: *mut EventBus, topic: &str, text: &str, err: &mut FfiError) -> i64 {
        let tags = abi::encode_value(&vec!["a".to_string()]);
        unsafe {
            events_events_EventBus_publish(
                bus,
                topic.as_ptr(),
                topic.len(),
                text.as_ptr(),
                text.len(),
                tags.as_ptr(),
                tags.len(),
                err,
            )
        }
    }

    #[test]
    fn subscribe_publish_and_iterate() {
        let mut err = FfiError::default();
        let freed = Arc::new(AtomicUsize::new(0));
        let bus = new_bus();

        let a = new_sub("quiet", "", &freed);
        let b = new_sub("", "", &freed);
        assert_eq!(subscribe(bus, a), 1);
        assert_eq!(subscribe(bus, b), 2);
        let a_state = unsafe { &*(a as *const SubState) };
        assert_eq!(a_state.attached.load(Ordering::SeqCst), 1);

        assert_eq!(publish(bus, "news", "hello", &mut err), 2);
        assert_eq!(publish(bus, "quiet", "psst", &mut err), 1);
        assert_eq!(publish(bus, "stop", "last", &mut err), 1);
        assert_eq!(err.code, 0);

        let iter = unsafe { events_events_EventBus_messages(bus, &mut err) };
        let mut got = Vec::new();
        loop {
            let mut item: *const u8 = std::ptr::null();
            let mut len = 0usize;
            let has = unsafe {
                events_events_EventBus_MessagesIterator_next(iter, &mut item, &mut len, &mut err)
            };
            if has == 0 {
                break;
            }
            got.push(unsafe { abi::lift_string(item, len) }.unwrap());
            unsafe { crate::events_free_bytes(item.cast_mut(), len) };
        }
        unsafe { events_events_EventBus_MessagesIterator_destroy(iter) };
        assert_eq!(got, vec!["hello", "psst", "last"]);

        let mut len = 0usize;
        let ptr = unsafe { events_events_EventBus_last_message(bus, &mut len, &mut err) };
        let last: Option<Message> =
            abi::decode_value(unsafe { std::slice::from_raw_parts(ptr, len) }).unwrap();
        unsafe { crate::events_free_bytes(ptr.cast_mut(), len) };
        assert_eq!(last.unwrap().text, "last");

        unsafe { events_events_EventBus_clear_subscribers(bus, &mut err) };
        assert_eq!(
            freed.load(Ordering::SeqCst),
            2,
            "free ran once per subscriber"
        );
        unsafe { events_events_EventBus_destroy(bus) };
    }

    #[test]
    fn a_failed_route_aborts_publish() {
        let mut err = FfiError::default();
        let freed = Arc::new(AtomicUsize::new(0));
        let bus = new_bus();
        subscribe(bus, new_sub("", "boom", &freed));
        publish(bus, "boom", "x", &mut err);
        assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
        assert_eq!(
            unsafe { err.message_str() },
            Some("subscriber rejected topic")
        );

        // The bus is still usable afterward.
        assert_eq!(publish(bus, "ok", "y", &mut err), 1);
        assert_eq!(err.code, 0);
        unsafe { events_events_EventBus_destroy(bus) };
        assert_eq!(
            freed.load(Ordering::SeqCst),
            1,
            "destroying the bus frees its subscriber"
        );
    }

    #[test]
    fn route_once_does_not_retain() {
        let mut err = FfiError::default();
        let freed = Arc::new(AtomicUsize::new(0));
        let topic = "quiet";
        let d = unsafe {
            events_events_route_once(
                new_sub("quiet", "", &freed),
                &VTABLE,
                topic.as_ptr(),
                topic.len(),
                &mut err,
            )
        };
        assert_eq!(d, Delivery::Skip as i32);
        assert_eq!(freed.load(Ordering::SeqCst), 1);

        let d = unsafe {
            events_events_route_once(
                new_sub("", "quiet", &freed),
                &VTABLE,
                topic.as_ptr(),
                topic.len(),
                &mut err,
            )
        };
        assert_eq!(d, 0);
        assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    }

    #[test]
    fn reference_counting() {
        let mut err = FfiError::default();
        let bus = new_bus();
        unsafe {
            let again = events_events_EventBus_clone(bus);
            assert_eq!(bus, again);
            events_events_EventBus_destroy(bus);
            assert_eq!(events_events_EventBus_subscriber_count(again, &mut err), 0);
            events_events_EventBus_destroy(again);
            events_events_EventBus_destroy(std::ptr::null_mut());
        }
    }
}
