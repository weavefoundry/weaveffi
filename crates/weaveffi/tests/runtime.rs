//! End-to-end runtime tests for the `#[weaveffi::module]` expansion.
//!
//! Each test defines a module with the macro, then calls the generated
//! `extern "C"` thunks directly (by their Rust path) and checks that
//! arguments lift, results lower, and errors flow through `out_err` the way
//! the C ABI promises. This is the executable proof that the generated glue
//! matches the calling convention every language binding expects. This test
//! crate is named `runtime`, so that's the prefix of every symbol.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use weaveffi::abi::{self, FfiError};

/// How long a test waits for an async completion. Generous, so a loaded
/// machine can't turn a slow completion into a failure; a passing run never
/// waits this long.
const WAIT: Duration = Duration::from_secs(30);

#[weaveffi::module]
pub mod demo {
    /// The demo module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum DemoError {
        /// division by zero
        DivisionByZero = 100,
    }

    impl std::fmt::Display for DemoError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::DivisionByZero => f.write_str("cannot divide by zero"),
            }
        }
    }

    /// A C-style enum that crosses the ABI as its `i32` discriminant.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Color {
        /// Red.
        Red = 0,
        /// Green.
        Green = 1,
        /// Blue.
        Blue = 2,
    }

    /// A by-value record with scalar, string, optional, and enum fields.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Point {
        /// The x coordinate.
        pub x: i32,
        /// A human-readable label.
        pub label: String,
        /// An optional nickname.
        pub nickname: Option<String>,
        /// The point's color.
        pub color: Color,
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Divide, surfacing division by zero as a domain error.
    #[weaveffi::export]
    pub fn checked_div(a: i32, b: i32) -> Result<i32, DemoError> {
        if b == 0 {
            return Err(DemoError::DivisionByZero);
        }
        Ok(a / b)
    }

    /// Greet by name (owned string in, owned string out).
    #[weaveffi::export]
    pub fn greet(name: String) -> String {
        format!("hi {name}")
    }

    /// Borrow a string slice and report its length in characters.
    #[weaveffi::export]
    pub fn str_len(text: &str) -> i32 {
        text.chars().count() as i32
    }

    /// The address of a borrowed string's first byte, proving the thunk lent
    /// the caller's bytes instead of copying them.
    #[weaveffi::export]
    pub fn str_addr(text: &str) -> u64 {
        text.as_ptr() as u64
    }

    /// The address of a borrowed byte slice's first byte.
    #[weaveffi::export]
    pub fn bytes_addr(data: &[u8]) -> u64 {
        data.as_ptr() as u64
    }

    /// Return an optional string depending on the flag.
    #[weaveffi::export]
    pub fn maybe_name(present: bool) -> Option<String> {
        present.then(|| "present".to_string())
    }

    /// Sum a list of scalars.
    #[weaveffi::export]
    pub fn sum(xs: Vec<i32>) -> i32 {
        xs.iter().sum()
    }

    /// Join a list of strings with a comma.
    #[weaveffi::export]
    pub fn join(parts: Vec<String>) -> String {
        parts.join(",")
    }

    /// Count bytes in an owned buffer.
    #[weaveffi::export]
    pub fn byte_count(data: Vec<u8>) -> i32 {
        data.len() as i32
    }

    /// Build a point by value (returned as a serialized value buffer).
    #[weaveffi::export]
    pub fn make_point(x: i32) -> Point {
        Point {
            x,
            label: "origin".to_string(),
            nickname: None,
            color: Color::Green,
        }
    }

    /// Read a point's x coordinate (record parameter by value).
    #[weaveffi::export]
    pub fn point_x(p: Point) -> i32 {
        p.x
    }

    /// Echo a color (C-style enum in and out).
    #[weaveffi::export]
    pub fn echo_color(c: Color) -> Color {
        c
    }
}

#[weaveffi::module]
pub mod warehouse {
    /// A record owned by the `warehouse` module.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Crate {
        /// Stable identifier.
        pub id: i64,
        /// Display label.
        pub label: String,
    }

    /// Build a crate by value.
    #[weaveffi::export]
    pub fn make_crate(id: i64, label: String) -> Crate {
        Crate { id, label }
    }
}

#[weaveffi::module]
pub mod dispatch {
    // A record declared in a *sibling* top-level module. The macro expands
    // each tree in isolation, lowers the unresolved name as a value buffer,
    // and asserts at compile time that `Crate` really is a by-value type.
    use super::warehouse::Crate;

    /// Read a sibling-module record's id (record parameter by value).
    #[weaveffi::export]
    pub fn crate_id(item: Crate) -> i64 {
        item.id
    }

    /// Return a relabeled copy (sibling-module record in and out).
    #[weaveffi::export]
    pub fn relabel(item: Crate, label: String) -> super::warehouse::Crate {
        Crate { id: item.id, label }
    }
}

/// Callback interfaces at full width: returns of every family adopted from
/// the consumer, a `throws` method decoded into the domain's typed error,
/// optional callback parameters, a type alias, and `#[cfg]`'d items.
#[weaveffi::module]
pub mod rich {
    use std::sync::Arc;

    use weaveffi::ForeignError;

    /// The lookup error domain; `Missing` carries the key.
    #[weaveffi::error]
    #[derive(Debug, PartialEq)]
    #[repr(i32)]
    pub enum LookupError {
        /// no such key
        Missing {
            /// The key that wasn't found.
            key: String,
        } = 1,
        /// the source is busy
        Busy = 2,
    }

    impl std::fmt::Display for LookupError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Missing { key } => write!(f, "missing {key}"),
                Self::Busy => f.write_str("busy"),
            }
        }
    }

    /// A key, spelled through an alias the macro substitutes.
    pub type Key = String;

    /// A record a callback returns.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Card {
        /// The card's name.
        pub name: String,
        /// Its tags.
        pub tags: Vec<String>,
    }

    /// An object a callback returns.
    #[weaveffi::interface]
    pub struct Token {
        n: i64,
    }

    impl Token {
        /// Create a token.
        pub fn new(n: i64) -> Self {
            Self { n }
        }
        /// Read the number.
        pub fn n(&self) -> i64 {
            self.n
        }
    }

    // Compiled out: neither the member thunk nor its contract entry exist.
    #[cfg(not(test))]
    impl Token {
        /// Never exported.
        pub fn hidden(&self) -> i64 {
            0
        }
    }

    /// A consumer-implemented source of values of every family.
    #[weaveffi::callback_interface]
    pub trait Source: Send + Sync {
        /// A string, returned as a consumer-allocated run.
        fn name(&self) -> Result<String, ForeignError>;
        /// Bytes, returned as a consumer-allocated run.
        fn blob(&self) -> Result<Vec<u8>, ForeignError>;
        /// A record, returned as a consumer-allocated value buffer.
        fn card(&self, key: &str) -> Result<Card, ForeignError>;
        /// An object, returned as one strong reference.
        fn token(&self) -> Result<Arc<Token>, ForeignError>;
        /// An optional object.
        fn maybe_token(&self) -> Result<Option<Arc<Token>>, ForeignError>;
        /// Look up a key, failing with a `LookupError`.
        #[weaveffi::throws]
        fn lookup(&self, key: &str) -> Result<i64, ForeignError>;
    }

    /// Call every value-returning method and describe the results.
    #[weaveffi::export]
    pub fn describe(source: Arc<dyn Source>) -> Result<String, ForeignError> {
        let card = source.card("k")?;
        let token = source.token()?;
        Ok(format!(
            "{} {:?} {}:{:?} {} {}",
            source.name()?,
            source.blob()?,
            card.name,
            card.tags,
            token.n(),
            source.maybe_token()?.is_some()
        ))
    }

    /// Look up `key` and describe the typed outcome.
    #[weaveffi::export]
    pub fn lookup_via(source: Arc<dyn Source>, key: Key) -> String {
        match source.lookup(&key) {
            Ok(v) => format!("ok {v}"),
            Err(e) => match e.domain::<LookupError>() {
                Some(LookupError::Missing { key }) => format!("missing {key}"),
                Some(LookupError::Busy) => "busy".to_string(),
                None => format!("foreign {}: {}", e.code, e.message),
            },
        }
    }

    /// Whether a source was passed (an optional callback parameter).
    #[weaveffi::export]
    pub fn has_source(source: Option<Arc<dyn Source>>, label: &str) -> String {
        format!("{label}:{}", source.is_some())
    }

    /// Present in every build of this test.
    #[cfg(test)]
    #[weaveffi::export]
    pub fn always() -> i32 {
        1
    }

    /// Present in no build: no thunk, no contract entry.
    #[cfg(not(test))]
    #[weaveffi::export]
    pub fn never() -> i32 {
        0
    }

    /// A submodule present in no build.
    #[cfg(not(test))]
    #[weaveffi::module]
    pub mod gone {
        /// Never exported.
        #[weaveffi::export]
        pub fn vanished() {}
    }
}

#[weaveffi::module]
pub mod maps {
    use std::collections::BTreeMap;

    /// Double every value in a string-keyed map (map in, map out).
    #[weaveffi::export]
    pub fn double_scores(scores: BTreeMap<String, i32>) -> BTreeMap<String, i32> {
        scores.into_iter().map(|(k, v)| (k, v * 2)).collect()
    }

    /// Sum a map's values (map parameter, scalar return).
    #[weaveffi::export]
    pub fn total(scores: BTreeMap<String, i32>) -> i32 {
        scores.values().sum()
    }
}

#[weaveffi::module]
pub mod build {
    /// A record whose fields exercise strings, scalars, and optionals in the
    /// value-buffer encoding.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Widget {
        /// Required display name.
        pub name: String,
        /// Quantity on hand.
        pub qty: i32,
        /// Optional shelf note.
        pub note: Option<String>,
    }
}

#[weaveffi::module]
pub mod geom {
    /// An algebraic shape: variants carry associated data, so it crosses the
    /// ABI as a value buffer (an `i32` tag followed by the active variant's
    /// fields).
    #[weaveffi::enumeration]
    #[derive(Clone, Debug, PartialEq)]
    pub enum Shape {
        /// The empty shape (a unit variant, tag 0).
        Empty,
        /// A circle with a radius (tag 1).
        Circle {
            /// The radius.
            radius: f64,
        },
        /// A labeled count (tag 2, by declaration order).
        Labeled {
            /// The label text.
            label: String,
            /// The count.
            count: u8,
        },
    }

    /// Describe a shape (rich enum borrowed in, owned string out).
    #[weaveffi::export]
    pub fn describe(shape: &Shape) -> String {
        match shape {
            Shape::Empty => "empty".to_string(),
            Shape::Circle { radius } => format!("circle({radius})"),
            Shape::Labeled { label, count } => format!("{label}x{count}"),
        }
    }
}

#[weaveffi::module]
pub mod stream {
    /// Yield `count` greetings lazily as an `iter<String>`.
    #[weaveffi::export]
    pub fn greetings(count: i32) -> weaveffi::Iter<String> {
        weaveffi::Iter::new((0..count).map(|i| format!("hi {i}")))
    }

    /// Yield the squares `0..count` lazily as an `iter<i32>`.
    #[weaveffi::export]
    pub fn squares(count: i32) -> weaveffi::Iter<i32> {
        weaveffi::Iter::new((0..count).map(|i| i * i))
    }

    /// The handle `reentrant`'s iterator advances from inside its own
    /// `next` (set by the test once the launcher returns it).
    pub static REENTRANT_HANDLE: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// Yield three elements, each the error code a nested `_next` on the
    /// same handle reported from inside the producer's `next`.
    #[weaveffi::export]
    pub fn reentrant() -> weaveffi::Iter<i32> {
        weaveffi::Iter::new((0..3).map(|_| {
            let handle = REENTRANT_HANDLE.load(std::sync::atomic::Ordering::SeqCst);
            let mut err = weaveffi::abi::FfiError::default();
            let mut item = 0;
            unsafe {
                runtime_stream_ReentrantIterator_next(
                    handle as *const weaveffi::abi::IterHandle<i32>,
                    &mut item,
                    &mut err,
                )
            };
            err.code
        }))
    }
}

#[weaveffi::module]
pub mod bus {
    use std::sync::{Arc, Mutex, PoisonError};

    use weaveffi::ForeignError;

    /// The bus's error domain. A subscriber failure propagates with its own
    /// code; the domain lets the bus's calls throw.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum BusError {
        /// the bus is closed
        Closed = 1,
    }

    impl std::fmt::Display for BusError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("the bus is closed")
        }
    }

    /// A message priority (a C-style enum crossing a callback boundary).
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Priority {
        /// Routine.
        Low = 0,
        /// Urgent.
        High = 1,
    }

    /// A message payload (a record crossing a callback boundary).
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Envelope {
        /// Sequence number.
        pub seq: i64,
        /// Optional topic.
        pub topic: Option<String>,
    }

    /// The consumer-implemented subscriber.
    #[weaveffi::callback_interface]
    pub trait Subscriber: Send + Sync {
        /// Receive a message; returns the subscriber's running total.
        fn on_message(
            &self,
            text: String,
            weight: i32,
            envelope: &Envelope,
        ) -> Result<i64, ForeignError>;
        /// Ask the subscriber how urgent it considers `weight`.
        fn classify(&self, weight: i32) -> Result<Priority, ForeignError>;
        /// Inspect a shared object without retaining it.
        fn on_ticker(
            &self,
            ticker: Arc<Ticker>,
            alt: Option<Arc<Ticker>>,
        ) -> Result<bool, ForeignError>;
        /// Weigh a message; a consumer failure comes back as an `Err`.
        fn weigh(&self, weight: i32) -> Result<i64, ForeignError>;
    }

    /// A shared object handed to subscribers.
    #[weaveffi::interface]
    pub struct Ticker {
        value: i64,
    }

    impl Ticker {
        /// Create a ticker.
        pub fn new(value: i64) -> Self {
            Self { value }
        }
        /// Read the value.
        pub fn value(&self) -> i64 {
            self.value
        }
    }

    /// A bus that retains its subscribers.
    #[weaveffi::interface]
    pub struct Bus {
        subs: Mutex<Vec<Arc<dyn Subscriber>>>,
    }

    impl Bus {
        /// Create an empty bus.
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                subs: Mutex::new(Vec::new()),
            })
        }

        /// Retain a subscriber.
        pub fn subscribe(&self, subscriber: Arc<dyn Subscriber>) {
            self.subs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(subscriber);
        }

        /// Publish to every subscriber, returning the sum of their totals. A
        /// subscriber failure fails the call with the subscriber's code and
        /// message. The list is snapshotted so no lock is held while a
        /// callback runs.
        pub fn publish(&self, text: &str, weight: i32) -> Result<i64, ForeignError> {
            let env = Envelope {
                seq: 1,
                topic: Some("t".to_string()),
            };
            let subs: Vec<Arc<dyn Subscriber>> = self
                .subs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            subs.iter()
                .map(|s| s.on_message(text.to_string(), weight, &env))
                .sum()
        }

        /// Publish asynchronously.
        pub async fn publish_later(&self, text: String, weight: i32) -> Result<i64, ForeignError> {
            self.publish(&text, weight)
        }

        /// Drop every subscriber (the consumer's `free` must run).
        pub fn clear(&self) {
            self.subs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
    }

    /// Call the subscriber once directly without retaining it.
    #[weaveffi::export]
    pub fn classify_once(
        subscriber: Arc<dyn Subscriber>,
        weight: i32,
    ) -> Result<Priority, ForeignError> {
        subscriber.classify(weight)
    }

    /// Hand the subscriber a ticker object.
    #[weaveffi::export]
    pub fn tick(subscriber: &Arc<dyn Subscriber>, value: i64) -> Result<bool, ForeignError> {
        let ticker = Arc::new(Ticker::new(value));
        Ok(subscriber.on_ticker(ticker.clone(), None)?
            && subscriber.on_ticker(ticker.clone(), Some(ticker))?)
    }

    /// Weigh through the subscriber and describe the outcome, handling a
    /// consumer failure as an ordinary value.
    #[weaveffi::export]
    pub fn weigh_or_explain(subscriber: Arc<dyn Subscriber>, weight: i32) -> String {
        match subscriber.weigh(weight) {
            Ok(v) => format!("ok {v}"),
            Err(e) => format!("err {}: {}", e.code, e.message),
        }
    }
}

#[weaveffi::module]
pub mod tasks {
    /// A tag for the thread running the call, to observe the executor.
    #[weaveffi::export]
    pub async fn thread_tag() -> String {
        let t = std::thread::current();
        format!("{:?} {}", t.id(), t.name().unwrap_or(""))
    }

    /// The task module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// arithmetic overflow
        Overflow = 1,
    }

    impl std::fmt::Display for TaskError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("arithmetic overflow")
        }
    }

    /// The by-value result an async task completes with.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct TaskResult {
        /// The assigned task id.
        pub id: i64,
        /// A human-readable completion message.
        pub value: String,
    }

    /// Run a named task asynchronously, completing with a `TaskResult`.
    #[weaveffi::export]
    pub async fn run_task(name: String) -> TaskResult {
        TaskResult {
            id: 7,
            value: format!("done: {name}"),
        }
    }

    /// Echo a string asynchronously (a `(result_ptr, result_len)` result).
    #[weaveffi::export]
    pub async fn shout(text: &str) -> String {
        text.to_uppercase()
    }

    /// Add two integers asynchronously, failing on overflow.
    #[weaveffi::export]
    pub async fn checked_add(a: i32, b: i32) -> Result<i32, TaskError> {
        a.checked_add(b).ok_or(TaskError::Overflow)
    }

    /// Never finish on its own: only cancellation completes this call.
    #[weaveffi::export]
    #[weaveffi::cancellable]
    pub async fn wait_forever(cancel: weaveffi::CancelToken) -> i32 {
        let _keep = cancel;
        std::future::pending::<()>().await;
        0
    }
}

fn ok_err() -> FfiError {
    FfiError::default()
}

/// The message in `err`, or `""`.
fn message(err: &FfiError) -> String {
    unsafe { err.message_str() }.unwrap_or_default().to_string()
}

/// Decode a buffered return `(ptr, out_len)` into an owned value and release
/// the producer-allocated buffer, mirroring what every generated binding does.
fn decode_ret<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
    assert!(
        !ptr.is_null(),
        "buffered return must not be null on success"
    );
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    // SAFETY: a producer's return; its tokens are ours to adopt, once.
    let value = unsafe { abi::decode_value(bytes) }.expect("well-formed value buffer");
    unsafe { abi::free_bytes(ptr.cast_mut(), len) };
    value
}

/// Copy a returned `(ptr, len)` string and release it.
fn take_string(ptr: *const u8, len: usize) -> String {
    let s = unsafe { abi::lift_string(ptr, len) }.expect("valid UTF-8");
    unsafe { abi::free_bytes(ptr.cast_mut(), len) };
    s
}

/// Copy a consumer-owned async error and release it.
fn take_async_err(err: *mut FfiError) -> (i32, String) {
    if err.is_null() {
        return (0, String::new());
    }
    let out = unsafe { ((*err).code, message(&*err)) };
    unsafe { abi::error_free(err) };
    out
}

#[test]
fn scalar_call_sets_ok() {
    let mut err = ok_err();
    let r = unsafe { demo::runtime_demo_add(2, 40, &mut err) };
    assert_eq!(r, 42);
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());
}

#[test]
fn fallible_ok_and_err_paths_use_display_for_the_message() {
    let mut err = ok_err();
    assert_eq!(
        unsafe { demo::runtime_demo_checked_div(10, 2, &mut err) },
        5
    );
    assert_eq!(err.code, 0);

    let r = unsafe { demo::runtime_demo_checked_div(1, 0, &mut err) };
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(
        err.code, 100,
        "domain code from the #[weaveffi::error] enum"
    );
    assert_eq!(message(&err), "cannot divide by zero", "the Display output");

    // The next successful call resets the slot.
    assert_eq!(unsafe { demo::runtime_demo_add(1, 1, &mut err) }, 2);
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());
}

#[test]
fn owned_string_roundtrip() {
    let mut err = ok_err();
    let input = "alice";
    let mut out_len = 0usize;
    let out =
        unsafe { demo::runtime_demo_greet(input.as_ptr(), input.len(), &mut out_len, &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(out, out_len), "hi alice");
}

#[test]
fn interior_nul_in_a_returned_string_round_trips() {
    let mut err = ok_err();
    let input = "a\0b";
    let mut out_len = 0usize;
    let out =
        unsafe { demo::runtime_demo_greet(input.as_ptr(), input.len(), &mut out_len, &mut err) };
    assert_eq!(out_len, 6);
    assert_eq!(take_string(out, out_len), "hi a\0b");
}

#[test]
fn borrowed_str_param_is_not_copied() {
    let mut err = ok_err();
    // A slice of a larger buffer: not NUL-terminated, and borrowed in place.
    let backing = "héllo world";
    let text = &backing[..6];
    assert_eq!(
        unsafe { demo::runtime_demo_str_len(text.as_ptr(), text.len(), &mut err) },
        5
    );
    let addr = unsafe { demo::runtime_demo_str_addr(text.as_ptr(), text.len(), &mut err) };
    assert_eq!(
        addr,
        text.as_ptr() as u64,
        "the thunk lent the caller's bytes"
    );

    let data = [1u8, 2, 3];
    let addr = unsafe { demo::runtime_demo_bytes_addr(data.as_ptr(), data.len(), &mut err) };
    assert_eq!(addr, data.as_ptr() as u64);

    // An empty string may be passed as (NULL, 0).
    assert_eq!(
        unsafe { demo::runtime_demo_str_len(std::ptr::null(), 0, &mut err) },
        0
    );
    assert_eq!(err.code, 0);
}

#[test]
fn invalid_string_params_are_marshalling_errors() {
    let mut err = ok_err();
    let bad = [0xFFu8, 0xFE];
    let r = unsafe { demo::runtime_demo_str_len(bad.as_ptr(), bad.len(), &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);

    let r = unsafe { demo::runtime_demo_str_len(std::ptr::null(), 3, &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
}

#[test]
fn optional_string_return_is_buffered() {
    let mut err = ok_err();
    let mut out_len: usize = 0;
    let some = unsafe { demo::runtime_demo_maybe_name(true, &mut out_len, &mut err) };
    assert_eq!(
        decode_ret::<Option<String>>(some, out_len),
        Some("present".to_string())
    );

    let none = unsafe { demo::runtime_demo_maybe_name(false, &mut out_len, &mut err) };
    assert_eq!(decode_ret::<Option<String>>(none, out_len), None);
}

#[test]
fn scalar_list_param_is_buffered() {
    let mut err = ok_err();
    let xs = abi::encode_value(&vec![3i32, 4, 5]);
    let total = unsafe { demo::runtime_demo_sum(xs.as_ptr(), xs.len(), &mut err) };
    assert_eq!(total, 12);
}

#[test]
fn string_list_param_is_buffered() {
    let mut err = ok_err();
    let parts = abi::encode_value(&vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    let mut out_len = 0usize;
    let out =
        unsafe { demo::runtime_demo_join(parts.as_ptr(), parts.len(), &mut out_len, &mut err) };
    assert_eq!(take_string(out, out_len), "a,b,c");
}

#[test]
fn malformed_buffer_param_reports_error() {
    let mut err = ok_err();
    // A truncated encoding (count with no elements) must be rejected through
    // `out_err`, never decoded partially.
    let bad = [9u8, 0, 0, 0];
    let total = unsafe { demo::runtime_demo_sum(bad.as_ptr(), bad.len(), &mut err) };
    assert_eq!(total, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
}

#[test]
fn byte_buffer_param() {
    let mut err = ok_err();
    let data = [1u8, 2, 3, 4, 5];
    assert_eq!(
        unsafe { demo::runtime_demo_byte_count(data.as_ptr(), data.len(), &mut err) },
        5
    );
}

#[test]
fn c_style_enum_in_and_out() {
    let mut err = ok_err();
    assert_eq!(unsafe { demo::runtime_demo_echo_color(2, &mut err) }, 2);
    assert_eq!(err.code, 0);
    assert_eq!(unsafe { demo::runtime_demo_echo_color(9, &mut err) }, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
}

#[test]
fn record_buffer_round_trip() {
    // A record is a value type: its whole generated surface is the
    // `BufferValue` impl, so encoding and decoding must round-trip every
    // field (including the optional and enum fields).
    let original = demo::Point {
        x: 7,
        label: "corner".to_string(),
        nickname: Some("nw".to_string()),
        color: demo::Color::Blue,
    };
    let bytes = abi::encode_value(&original);
    assert_eq!(bytes.len(), abi::BufferValue::encoded_len(&original));
    let back: demo::Point = unsafe { abi::decode_value(&bytes) }.expect("round-trip");
    assert_eq!(back.x, 7);
    assert_eq!(back.label, "corner");
    assert_eq!(back.nickname.as_deref(), Some("nw"));
    assert_eq!(back.color, demo::Color::Blue);
}

#[test]
fn record_param_is_buffered() {
    let mut err = ok_err();
    let p = demo::Point {
        x: 41,
        label: "in".to_string(),
        nickname: None,
        color: demo::Color::Red,
    };
    let bytes = abi::encode_value(&p);
    let x = unsafe { demo::runtime_demo_point_x(bytes.as_ptr(), bytes.len(), &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(x, 41);
}

#[test]
fn struct_return_is_buffered() {
    let mut err = ok_err();
    let mut out_len: usize = 0;
    let ptr = unsafe { demo::runtime_demo_make_point(99, &mut out_len, &mut err) };
    assert_eq!(err.code, 0);
    let p: demo::Point = decode_ret(ptr, out_len);
    assert_eq!(p.x, 99);
    assert_eq!(p.label, "origin");
    assert_eq!(p.nickname, None);
    assert_eq!(p.color, demo::Color::Green);
}

#[test]
fn sibling_module_record_param_and_return() {
    let mut err = ok_err();
    let label = "widget";
    let mut out_len: usize = 0;
    let ptr = unsafe {
        warehouse::runtime_warehouse_make_crate(
            7,
            label.as_ptr(),
            label.len(),
            &mut out_len,
            &mut err,
        )
    };
    assert_eq!(err.code, 0);
    let c: warehouse::Crate = decode_ret(ptr, out_len);
    assert_eq!(c.id, 7);
    assert_eq!(c.label, "widget");

    // `dispatch::crate_id` accepts `warehouse::Crate` as a value buffer.
    let bytes = abi::encode_value(&c);
    let id = unsafe { dispatch::runtime_dispatch_crate_id(bytes.as_ptr(), bytes.len(), &mut err) };
    assert_eq!(id, 7);
    assert_eq!(err.code, 0);

    let new_label = "gadget";
    let ptr2 = unsafe {
        dispatch::runtime_dispatch_relabel(
            bytes.as_ptr(),
            bytes.len(),
            new_label.as_ptr(),
            new_label.len(),
            &mut out_len,
            &mut err,
        )
    };
    let c2: warehouse::Crate = decode_ret(ptr2, out_len);
    assert_eq!(c2.id, 7);
    assert_eq!(c2.label, "gadget");
}

/// FNV-1a 64, the contract table's hash.
fn fnv(data: &str) -> u64 {
    data.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A top-level module's contract table, as a consumer reads it.
fn contract(
    f: unsafe extern "C" fn(*mut usize) -> *const abi::ContractEntry,
) -> Vec<abi::ContractEntry> {
    let mut len = 0usize;
    let ptr = unsafe { f(&mut len) };
    if len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

#[test]
fn contract_tables_hold_one_sorted_entry_per_declaration() {
    let table = contract(demo::runtime_demo_contract);
    assert!(table.windows(2).all(|w| w[0].id < w[1].id), "sorted by id");
    // demo: the error domain, Color, Point, and thirteen functions.
    assert_eq!(table.len(), 16);
    let add = table.iter().find(|e| e.id == fnv("demo.add")).unwrap();
    assert_eq!(add.hash, fnv("function add(a: i32, b: i32) -> i32"));
    let point = table.iter().find(|e| e.id == fnv("demo.Point")).unwrap();
    assert_eq!(
        point.hash,
        fnv("record Point {x: i32, label: string, nickname: string?, color: Color}")
    );
    let div = table
        .iter()
        .find(|e| e.id == fnv("demo.checked_div"))
        .unwrap();
    assert_eq!(
        div.hash,
        fnv("function checked_div(a: i32, b: i32) -> i32 throws")
    );

    // Nested modules and interface members are in their root's table.
    let outer = contract(outer::runtime_outer_contract);
    for path in [
        "outer.Session",
        "outer.Session.open",
        "outer.inner.summarize",
    ] {
        assert!(outer.iter().any(|e| e.id == fnv(path)), "{path}");
    }
}

#[test]
fn contract_tables_follow_cfg() {
    let table = contract(rich::runtime_rich_contract);
    let has = |path: &str| table.iter().any(|e| e.id == fnv(path));
    assert!(has("rich.always"));
    assert!(has("rich.Token.n"));
    assert!(!has("rich.never"), "a cfg'd-out function has no entry");
    assert!(
        !has("rich.Token.hidden"),
        "nor a member of a cfg'd-out impl"
    );
    assert!(
        !has("rich.gone.vanished"),
        "nor anything in a cfg'd-out module"
    );
    let lookup = table
        .iter()
        .find(|e| e.id == fnv("rich.lookup_via"))
        .unwrap();
    assert_eq!(
        lookup.hash,
        fnv("function lookup_via(source: Source, key: string) -> string"),
        "the alias resolved to its target"
    );
}

#[test]
fn map_param_and_return_are_buffered() {
    use std::collections::BTreeMap;
    let mut err = ok_err();
    let mut scores = BTreeMap::new();
    scores.insert("a".to_string(), 2i32);
    scores.insert("b".to_string(), 1i32);
    let bytes = abi::encode_value(&scores);

    let mut out_len: usize = 0;
    let ptr = unsafe {
        maps::runtime_maps_double_scores(bytes.as_ptr(), bytes.len(), &mut out_len, &mut err)
    };
    assert_eq!(err.code, 0);
    let doubled: BTreeMap<String, i32> = decode_ret(ptr, out_len);
    assert_eq!(doubled.get("a"), Some(&4));
    assert_eq!(doubled.get("b"), Some(&2));

    let total = unsafe { maps::runtime_maps_total(bytes.as_ptr(), bytes.len(), &mut err) };
    assert_eq!(total, 3);
}

#[test]
fn widget_optional_field_round_trips() {
    for w in [
        build::Widget {
            name: "bolt".to_string(),
            qty: 7,
            note: Some("aisle 4".to_string()),
        },
        build::Widget {
            name: "nut".to_string(),
            qty: 1,
            note: None,
        },
    ] {
        let back: build::Widget = unsafe { abi::decode_value(&abi::encode_value(&w)) }.unwrap();
        assert_eq!(back, w);
    }
}

#[test]
fn rich_enum_encodes_tag_then_fields() {
    // The wire format leads with the i32 tag (declaration order: Empty = 0,
    // Circle = 1, Labeled = 2), then the active variant's fields.
    let empty = abi::encode_value(&geom::Shape::Empty);
    assert_eq!(empty, [0, 0, 0, 0]);

    let circle = abi::encode_value(&geom::Shape::Circle { radius: 2.5 });
    assert_eq!(&circle[..4], [1, 0, 0, 0]);
    assert_eq!(circle.len(), 4 + 8, "tag + f64 radius");

    let labeled = geom::Shape::Labeled {
        label: "hex".to_string(),
        count: 6,
    };
    let bytes = abi::encode_value(&labeled);
    assert_eq!(&bytes[..4], [2, 0, 0, 0]);
    let back: geom::Shape = unsafe { abi::decode_value(&bytes) }.unwrap();
    assert_eq!(back, labeled);

    // An out-of-range tag is a decode error, not a silent default.
    assert!(unsafe { abi::decode_value::<geom::Shape>(&[9, 0, 0, 0]) }.is_err());
}

#[test]
fn rich_enum_param_is_buffered() {
    let mut err = ok_err();
    let circle = abi::encode_value(&geom::Shape::Circle { radius: 2.5 });
    let mut out_len = 0usize;
    let d = unsafe {
        geom::runtime_geom_describe(circle.as_ptr(), circle.len(), &mut out_len, &mut err)
    };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(d, out_len), "circle(2.5)");
}

#[test]
fn iterator_string_elements() {
    let mut err = ok_err();
    let iter = unsafe { stream::runtime_stream_greetings(3, &mut err) };
    assert_eq!(err.code, 0);
    assert!(!iter.is_null());

    let mut got = Vec::new();
    loop {
        let mut item: *const u8 = std::ptr::null();
        let mut len = 0usize;
        let has = unsafe {
            stream::runtime_stream_GreetingsIterator_next(iter, &mut item, &mut len, &mut err)
        };
        assert_eq!(err.code, 0);
        if has == 0 {
            break;
        }
        got.push(take_string(item, len));
    }
    unsafe { stream::runtime_stream_GreetingsIterator_destroy(iter) };
    assert_eq!(got, vec!["hi 0", "hi 1", "hi 2"]);
}

#[test]
fn iterator_scalar_elements() {
    let mut err = ok_err();
    let iter = unsafe { stream::runtime_stream_squares(4, &mut err) };
    let mut got = Vec::new();
    loop {
        let mut item: i32 = 0;
        if unsafe { stream::runtime_stream_SquaresIterator_next(iter, &mut item, &mut err) } == 0 {
            break;
        }
        got.push(item);
    }
    unsafe { stream::runtime_stream_SquaresIterator_destroy(iter) };
    assert_eq!(got, vec![0, 1, 4, 9]);
}

#[test]
fn concurrent_iterator_next_yields_each_element_once_or_reports_busy() {
    const N: i32 = 2000;
    let mut err = ok_err();
    let iter = unsafe { stream::runtime_stream_squares(N, &mut err) };
    let addr = iter as usize;
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                let iter = addr as *mut abi::IterHandle<i32>;
                let mut err = FfiError::default();
                let mut got = Vec::new();
                loop {
                    let mut item = 0i32;
                    let has = unsafe {
                        stream::runtime_stream_SquaresIterator_next(iter, &mut item, &mut err)
                    };
                    // A `_next` racing another fails instead of blocking.
                    if err.code == abi::MARSHAL_ERROR_CODE {
                        std::thread::yield_now();
                        continue;
                    }
                    assert_eq!(err.code, 0);
                    if has == 0 {
                        break got;
                    }
                    got.push(item);
                }
            })
        })
        .collect();
    let mut all: Vec<i32> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    all.sort_unstable();
    assert_eq!(all, (0..N).map(|i| i * i).collect::<Vec<_>>());
    unsafe { stream::runtime_stream_SquaresIterator_destroy(iter) };
}

#[test]
fn a_reentrant_next_fails_instead_of_deadlocking() {
    let mut err = ok_err();
    let iter = unsafe { stream::runtime_stream_reentrant(&mut err) };
    stream::REENTRANT_HANDLE.store(iter as usize, std::sync::atomic::Ordering::SeqCst);
    let mut got = Vec::new();
    loop {
        let mut item = 0i32;
        if unsafe { stream::runtime_stream_ReentrantIterator_next(iter, &mut item, &mut err) } == 0
        {
            break;
        }
        assert_eq!(err.code, 0);
        got.push(item);
    }
    unsafe { stream::runtime_stream_ReentrantIterator_destroy(iter) };
    assert_eq!(got, vec![abi::MARSHAL_ERROR_CODE; 3]);
}

#[test]
fn duplicate_map_keys_are_marshalling_errors() {
    let mut err = ok_err();
    let mut w = abi::BufferWriter::new();
    w.write_len(2);
    for v in [1i32, 2] {
        w.write_string("same");
        w.write_i32(v);
    }
    let bytes = w.finish();
    let total = unsafe { maps::runtime_maps_total(bytes.as_ptr(), bytes.len(), &mut err) };
    assert_eq!(total, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    assert!(
        message(&err).contains("duplicate map key"),
        "{}",
        message(&err)
    );
}

/// A consumer-side `Subscriber` implementation: the context is a heap-allocated
/// `SubState`, the vtable is a process-wide static, exactly as a generated
/// binding would do it.
mod consumer_subscriber {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    pub struct SubState {
        pub total: AtomicI64,
        pub fail_at: i32,
        pub last_topic: std::sync::Mutex<Option<String>>,
        pub freed: Arc<AtomicUsize>,
    }

    unsafe extern "C" fn on_message(
        ctx: *mut c_void,
        text_ptr: *const u8,
        text_len: usize,
        weight: i32,
        envelope_ptr: *const u8,
        envelope_len: usize,
        out_err: *mut FfiError,
    ) -> i64 {
        let state = unsafe { &*(ctx as *const SubState) };
        let text = unsafe { abi::lift_str(text_ptr, text_len) }.unwrap();
        let env: bus::Envelope =
            unsafe { abi::decode_value(std::slice::from_raw_parts(envelope_ptr, envelope_len)) }
                .unwrap();
        *state.last_topic.lock().unwrap() = env.topic.clone();
        if weight == state.fail_at {
            let msg = std::ffi::CString::new(format!("subscriber rejected {text}")).unwrap();
            // Consumers report through the exported `{prefix}_error_set`.
            unsafe { super::runtime_error_set(out_err, abi::FOREIGN_ERROR_CODE, msg.as_ptr()) };
            return 0;
        }
        state.total.fetch_add(weight as i64, Ordering::Relaxed) + weight as i64
    }

    unsafe extern "C" fn classify(_ctx: *mut c_void, weight: i32, _err: *mut FfiError) -> i32 {
        i32::from(weight > 5)
    }

    unsafe extern "C" fn on_ticker(
        _ctx: *mut c_void,
        ticker: *mut bus::Ticker,
        alt: *mut bus::Ticker,
        _err: *mut FfiError,
    ) -> bool {
        // Object arguments transfer one strong reference: the consumer adopts
        // each non-null pointer and owes exactly one `_destroy`.
        let mut err = FfiError::default();
        unsafe {
            let v = bus::runtime_bus_Ticker_value(ticker, &mut err);
            bus::runtime_bus_Ticker_destroy(ticker);
            let alt_ok = if alt.is_null() {
                true
            } else {
                let same = bus::runtime_bus_Ticker_value(alt, &mut err) == v;
                bus::runtime_bus_Ticker_destroy(alt);
                same
            };
            v == 42 && alt_ok
        }
    }

    unsafe extern "C" fn weigh(ctx: *mut c_void, weight: i32, out_err: *mut FfiError) -> i64 {
        let state = unsafe { &*(ctx as *const SubState) };
        if weight == state.fail_at {
            unsafe { abi::error_set(out_err, abi::FOREIGN_ERROR_CODE, "too heavy") };
            return 0;
        }
        i64::from(weight) * 10
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        let state = unsafe { Box::from_raw(ctx as *mut SubState) };
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    pub static VTABLE: bus::runtime_bus_Subscriber_vtable = bus::runtime_bus_Subscriber_vtable {
        header: abi::VtableHeader {
            size: std::mem::size_of::<bus::runtime_bus_Subscriber_vtable>() as u32,
            flags: 0,
            free,
        },
        on_message,
        classify,
        on_ticker,
        weigh,
    };

    /// A vtable whose `size` claims only the header: a consumer built from
    /// an older contract.
    pub static SHORT_VTABLE: bus::runtime_bus_Subscriber_vtable =
        bus::runtime_bus_Subscriber_vtable {
            header: abi::VtableHeader {
                size: std::mem::size_of::<abi::VtableHeader>() as u32,
                flags: 0,
                free,
            },
            on_message,
            classify,
            on_ticker,
            weigh,
        };

    pub fn new_ctx(fail_at: i32, freed: &Arc<AtomicUsize>) -> *mut c_void {
        Box::into_raw(Box::new(SubState {
            total: AtomicI64::new(0),
            fail_at,
            last_topic: std::sync::Mutex::new(None),
            freed: Arc::clone(freed),
        })) as *mut c_void
    }
}

weaveffi::export_runtime!();

#[test]
fn callback_interface_sync_paths() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = ok_err();

    // Direct-family return through a non-retained callback: `free` runs as soon
    // as the thunk drops its `Arc<dyn Subscriber>`.
    let ctx = new_ctx(-1, &freed);
    assert_eq!(
        unsafe { bus::runtime_bus_classify_once(ctx, &VTABLE, 9, &mut err) },
        1
    );
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 1);

    // Objects flow producer -> consumer as owned references; a
    // `&Arc<dyn Trait>` spelling lends the lifted callback.
    let ctx = new_ctx(-1, &freed);
    assert!(unsafe { bus::runtime_bus_tick(ctx, &VTABLE, 42, &mut err) });
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 2);

    // A null vtable is a marshalling error, not a crash.
    let r = unsafe {
        bus::runtime_bus_classify_once(std::ptr::null_mut(), std::ptr::null(), 1, &mut err)
    };
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
}

#[test]
fn a_vtable_smaller_than_the_producers_is_rejected_and_released() {
    use consumer_subscriber::{new_ctx, SHORT_VTABLE};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = ok_err();
    let r =
        unsafe { bus::runtime_bus_classify_once(new_ctx(-1, &freed), &SHORT_VTABLE, 9, &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    assert!(
        message(&err).contains("regenerate the bindings"),
        "{}",
        message(&err)
    );
    assert_eq!(freed.load(Ordering::SeqCst), 1, "the context was released");
}

#[test]
fn callback_result_methods_return_failures_as_values() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = ok_err();
    let mut len = 0usize;

    let ptr = unsafe {
        bus::runtime_bus_weigh_or_explain(new_ctx(3, &freed), &VTABLE, 2, &mut len, &mut err)
    };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(ptr, len), "ok 20");

    // The consumer fails, the producer sees an `Err`, and the call itself
    // succeeds: nothing unwound.
    let ptr = unsafe {
        bus::runtime_bus_weigh_or_explain(new_ctx(3, &freed), &VTABLE, 3, &mut len, &mut err)
    };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(ptr, len), "err -4: too heavy");
    assert_eq!(freed.load(Ordering::SeqCst), 2);
}

#[test]
fn callback_interface_retained_and_foreign_error() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = ok_err();

    let b = unsafe { bus::runtime_bus_Bus_new(&mut err) };
    assert!(!b.is_null());
    unsafe {
        bus::runtime_bus_Bus_subscribe(b, new_ctx(7, &freed), &VTABLE, &mut err);
        bus::runtime_bus_Bus_subscribe(b, new_ctx(-1, &freed), &VTABLE, &mut err);
    }
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 0, "retained by the bus");

    let text = "hi";
    let publish = |weight: i32, err: &mut FfiError| unsafe {
        bus::runtime_bus_Bus_publish(b, text.as_ptr(), text.len(), weight, err)
    };
    assert_eq!(publish(3, &mut err), 6);
    assert_eq!(err.code, 0);
    assert_eq!(publish(5, &mut err), 16);

    // The first subscriber fails on weight 7: the producer call is aborted and
    // the consumer's own message comes back with FOREIGN_ERROR_CODE.
    assert_eq!(publish(7, &mut err), 0);
    assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(message(&err), "subscriber rejected hi");

    // The bus is still usable afterwards.
    assert_eq!(publish(1, &mut err), 18);
    assert_eq!(err.code, 0);

    unsafe { bus::runtime_bus_Bus_clear(b, &mut err) };
    assert_eq!(freed.load(Ordering::SeqCst), 2, "free runs once each");
    unsafe { bus::runtime_bus_Bus_destroy(b) };
}

type Completion<T> = mpsc::Sender<(i32, String, T)>;

/// Box a sender as an async `context`; reclaim it with [`drop_ctx`].
fn new_ctx<T>(tx: Completion<T>) -> *mut c_void {
    Box::into_raw(Box::new(tx)).cast()
}

fn drop_ctx<T>(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<Completion<T>>()) });
}

fn send<T>(ctx: *mut c_void, err: *mut FfiError, value: T) {
    // Clone the sender before sending: the test may free the context as soon
    // as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<Completion<T>>() }.clone();
    let (code, msg) = take_async_err(err);
    tx.send((code, msg, value)).unwrap();
}

extern "C" fn on_i64(ctx: *mut c_void, err: *mut FfiError, result: i64) {
    send(ctx, err, result);
}

extern "C" fn on_i32(ctx: *mut c_void, err: *mut FfiError, result: i32) {
    send(ctx, err, result);
}

extern "C" fn on_string(ctx: *mut c_void, err: *mut FfiError, ptr: *const u8, len: usize) {
    let value = if ptr.is_null() {
        String::new()
    } else {
        take_string(ptr, len)
    };
    send(ctx, err, value);
}

#[test]
fn callback_interface_from_async_method() {
    use consumer_subscriber::{new_ctx as sub_ctx, VTABLE};

    let mut err = ok_err();
    let b = unsafe { bus::runtime_bus_Bus_new(&mut err) };
    let freed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    unsafe { bus::runtime_bus_Bus_subscribe(b, sub_ctx(4, &freed), &VTABLE, &mut err) };

    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i64>(tx);
    let text = "async";

    unsafe { bus::runtime_bus_Bus_publish_later(b, text.as_ptr(), text.len(), 2, on_i64, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 2));

    // A foreign failure inside the future is delivered through the callback.
    unsafe { bus::runtime_bus_Bus_publish_later(b, text.as_ptr(), text.len(), 4, on_i64, ctx) };
    let (code, msg, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(msg, "subscriber rejected async");
    assert_eq!(result, 0);

    // The receiver was retained across the spawn: releasing the consumer's
    // reference while a call is in flight is safe.
    unsafe {
        bus::runtime_bus_Bus_publish_later(b, text.as_ptr(), text.len(), 1, on_i64, ctx);
        bus::runtime_bus_Bus_destroy(b);
    }
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 3));
    drop_ctx::<i64>(ctx);
}

#[test]
fn async_struct_result_completes_via_callback() {
    type Msg = (i32, String, Option<(i64, String)>);
    // The buffered result is owned by the consumer: decode it, then release
    // the producer allocation with `free_bytes`.
    extern "C" fn cb(ctx: *mut c_void, err: *mut FfiError, ptr: *const u8, len: usize) {
        // Clone the sender before sending: the test may free the context as soon
        // as the value arrives, which can be before `send` returns.
        let tx = unsafe { &*ctx.cast::<mpsc::Sender<Msg>>() }.clone();
        let (code, msg) = take_async_err(err);
        let value = (!ptr.is_null()).then(|| {
            let r: tasks::TaskResult = decode_ret(ptr, len);
            (r.id, r.value)
        });
        tx.send((code, msg, value)).unwrap();
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    let name = "alpha";
    unsafe { tasks::runtime_tasks_run_task(name.as_ptr(), name.len(), cb, ctx) };
    let (code, _, value) = rx.recv_timeout(WAIT).unwrap();
    drop(unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<Msg>>()) });
    assert_eq!(code, 0);
    assert_eq!(value, Some((7, "done: alpha".to_string())));
}

#[test]
fn async_string_result_is_ptr_and_len() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<String>(tx);
    let text = "quiet\0please";
    unsafe { tasks::runtime_tasks_shout(text.as_ptr(), text.len(), on_string, ctx) };
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (0, String::new(), "QUIET\0PLEASE".into())
    );
    drop_ctx::<String>(ctx);
}

#[test]
fn async_result_ok_and_err_paths() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);

    unsafe { tasks::runtime_tasks_checked_add(2, 3, on_i32, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 5));

    unsafe { tasks::runtime_tasks_checked_add(i32::MAX, 1, on_i32, ctx) };
    let (code, msg, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!((code, msg.as_str(), result), (1, "arithmetic overflow", 0));

    drop_ctx::<i32>(ctx);
}

#[test]
fn cancelling_completes_with_the_cancelled_code_exactly_once() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    let token = runtime_cancel_token_create();
    unsafe { tasks::runtime_tasks_wait_forever(token, on_i32, ctx) };
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "not complete until cancelled"
    );
    unsafe {
        runtime_cancel_token_cancel(token);
        assert!(runtime_cancel_token_is_cancelled(token));
        runtime_cancel_token_destroy(token);
    }
    let (code, msg, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(
        (code, msg.as_str(), result),
        (abi::CANCELLED_ERROR_CODE, "cancelled", 0)
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop_ctx::<i32>(ctx);
}

#[test]
fn cancel_then_destroy_races_the_launch_safely() {
    const RUNS: usize = 200;
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    for i in 0..RUNS {
        let token = runtime_cancel_token_create();
        if i % 2 == 0 {
            // The launcher takes its own reference before returning, so the
            // consumer may then cancel and destroy its reference from another
            // thread while the spawned future starts running. (Destroying the
            // token before handing it to a launcher would break the contract:
            // the launcher would adopt a freed token.)
            unsafe { tasks::runtime_tasks_wait_forever(token, on_i32, ctx) };
            let raw = token as usize;
            let canceller = std::thread::spawn(move || unsafe {
                runtime_cancel_token_cancel(raw as *mut abi::FfiCancelToken);
                runtime_cancel_token_destroy(raw as *mut abi::FfiCancelToken);
            });
            canceller.join().unwrap();
        } else {
            // Cancel and destroy before the spawned future is ever polled.
            unsafe {
                tasks::runtime_tasks_wait_forever(token, on_i32, ctx);
                runtime_cancel_token_cancel(token);
                runtime_cancel_token_destroy(token);
            }
        }
    }
    for _ in 0..RUNS {
        let (code, _, _) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(code, abi::CANCELLED_ERROR_CODE);
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "each call completed exactly once"
    );
    drop_ctx::<i32>(ctx);
}

/// Exercises nested-module codegen: the inner module's symbols carry the
/// joined `outer_inner` path, and a nested function may reference an
/// interface declared in its parent module via `super::`.
#[weaveffi::module]
pub mod outer {
    use std::sync::Arc;

    /// An interface declared in the parent module.
    #[weaveffi::interface]
    pub struct Session {
        /// The session id.
        pub id: i64,
    }

    impl Session {
        /// Open a session.
        pub fn open(id: i64) -> Self {
            Self { id }
        }
    }

    /// A C-style enum declared in the parent module.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Level {
        /// Low.
        Low = 1,
        /// High.
        High = 2,
    }

    /// Return the same session (an `Arc<Self>`-typed parameter and return).
    #[weaveffi::export]
    pub fn share(session: Arc<Session>) -> Arc<Session> {
        session
    }

    /// The nested sub-module: its symbols use the `outer_inner` path.
    #[weaveffi::module]
    pub mod inner {
        use std::sync::Arc;

        /// A by-value record produced by the nested module.
        #[weaveffi::record]
        #[derive(Clone)]
        pub struct Report {
            /// Ten times the session id.
            pub score: i64,
            /// The session the report is about (an object token in the buffer).
            pub session: Option<Arc<super::Session>>,
        }

        /// Summarize a parent-module `Session` into a nested `Report`.
        #[weaveffi::export]
        pub fn summarize(session: &super::Session, keep: Option<&super::Session>) -> Report {
            Report {
                score: session.id * 10 + keep.map_or(0, |k| k.id),
                session: None,
            }
        }

        /// Attach a retained session to a report.
        #[weaveffi::export]
        pub fn attach(session: Arc<super::Session>) -> Report {
            Report {
                score: session.id,
                session: Some(session),
            }
        }

        /// Read back the session inside a report.
        #[weaveffi::export]
        pub fn session_of(report: Report) -> Option<Arc<super::Session>> {
            report.session
        }

        /// Raise a parent-module C-style enum (by value in and out).
        #[weaveffi::export]
        pub fn raise(level: super::Level) -> super::Level {
            let _ = level;
            super::Level::High
        }
    }
}

#[test]
fn nested_module_symbols_and_parent_type_reference() {
    let mut err = ok_err();
    let session = unsafe { outer::runtime_outer_Session_open(7, &mut err) };
    assert_eq!(err.code, 0);
    assert!(!session.is_null());

    let mut out_len: usize = 0;
    let ptr = unsafe {
        outer::inner::runtime_outer_inner_summarize(
            session,
            std::ptr::null(),
            &mut out_len,
            &mut err,
        )
    };
    assert_eq!(err.code, 0);
    let report: outer::inner::Report = decode_ret(ptr, out_len);
    assert_eq!(report.score, 70);
    assert!(report.session.is_none());

    let ptr = unsafe {
        outer::inner::runtime_outer_inner_summarize(session, session, &mut out_len, &mut err)
    };
    let report: outer::inner::Report = decode_ret(ptr, out_len);
    assert_eq!(report.score, 77);

    assert_eq!(
        unsafe { outer::inner::runtime_outer_inner_raise(1, &mut err) },
        2
    );
    unsafe { outer::runtime_outer_Session_destroy(session) };
}

#[test]
fn object_reference_counting() {
    let mut err = ok_err();
    unsafe {
        let s = outer::runtime_outer_Session_open(3, &mut err);

        // `share` retains through `Arc<Session>` in and hands back a new
        // strong reference; the pointer identity is the same allocation.
        let again = outer::runtime_outer_share(s, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(again, s, "the same object, one more reference");
        let third = outer::runtime_outer_Session_clone(s);
        assert_eq!(third, s);

        outer::runtime_outer_Session_destroy(s);
        outer::runtime_outer_Session_destroy(again);
        // Still alive through `third`.
        let mut out_len: usize = 0;
        let ptr = outer::inner::runtime_outer_inner_summarize(
            third,
            std::ptr::null(),
            &mut out_len,
            &mut err,
        );
        assert_eq!(err.code, 0);
        let report: outer::inner::Report = decode_ret(ptr, out_len);
        assert_eq!(report.score, 30);
        outer::runtime_outer_Session_destroy(third);
        outer::runtime_outer_Session_destroy(std::ptr::null_mut());
        assert!(outer::runtime_outer_Session_clone(std::ptr::null()).is_null());
    }
}

#[test]
fn objects_inside_value_buffers_carry_a_reference() {
    let mut err = ok_err();
    unsafe {
        let s = outer::runtime_outer_Session_open(5, &mut err);

        // `attach` retains the session inside the returned record: the
        // buffer's object token is one strong reference the consumer adopts.
        let mut out_len: usize = 0;
        let ptr = outer::inner::runtime_outer_inner_attach(s, &mut out_len, &mut err);
        assert_eq!(err.code, 0);
        let bytes = std::slice::from_raw_parts(ptr, out_len).to_vec();
        abi::free_bytes(ptr.cast_mut(), out_len);
        outer::runtime_outer_Session_destroy(s);

        // Sending the buffer back transfers the token's reference to the
        // producer, which returns it as the optional object result.
        let back =
            outer::inner::runtime_outer_inner_session_of(bytes.as_ptr(), bytes.len(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(back, s, "same allocation, still alive");
        outer::runtime_outer_Session_destroy(back);

        let none = abi::encode_value(&outer::inner::Report {
            score: 0,
            session: None,
        });
        let back =
            outer::inner::runtime_outer_inner_session_of(none.as_ptr(), none.len(), &mut err);
        assert!(back.is_null());
        assert_eq!(err.code, 0);
    }
}

/// A producer module whose fallible function reports through a hand-written
/// [`weaveffi::ErrorReport`] type rather than the domain enum itself.
#[weaveffi::module]
pub mod vault {
    use weaveffi::ErrorReport;

    /// The vault's declared error domain: the codes consumers can match on.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum VaultError {
        /// entry not found
        NotFound = 2001,
        /// vault sealed
        Sealed = 2002,
    }

    impl std::fmt::Display for VaultError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::NotFound => "entry not found",
                Self::Sealed => "vault sealed",
            })
        }
    }

    /// The producer's internal failure type: it carries data the declared
    /// domain doesn't, so it maps itself onto the domain's codes with a
    /// hand-written `ErrorReport` and dynamic messages.
    pub enum VaultFailure {
        /// No entry exists for the key.
        NotFound,
        /// The vault is sealed for the given reason.
        Sealed(String),
    }

    impl ErrorReport for VaultFailure {
        fn code(&self) -> i32 {
            match self {
                VaultFailure::NotFound => 2001,
                VaultFailure::Sealed(_) => 2002,
            }
        }
        fn message(&self) -> String {
            match self {
                VaultFailure::NotFound => "entry not found".to_string(),
                VaultFailure::Sealed(reason) => format!("vault sealed: {reason}"),
            }
        }
    }

    /// Fetch a doubled value, failing with a domain code for invalid keys.
    #[weaveffi::export]
    pub fn fetch(key: i64) -> Result<i64, VaultFailure> {
        match key {
            0 => Err(VaultFailure::NotFound),
            n if n < 0 => Err(VaultFailure::Sealed("negative key".to_string())),
            n => Ok(n * 2),
        }
    }
}

#[test]
fn fallible_with_domain_error_codes() {
    let mut err = ok_err();
    assert_eq!(unsafe { vault::runtime_vault_fetch(21, &mut err) }, 42);
    assert_eq!(err.code, 0);

    let r = unsafe { vault::runtime_vault_fetch(0, &mut err) };
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, 2001);
    assert_eq!(message(&err), "entry not found");

    let r = unsafe { vault::runtime_vault_fetch(-1, &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, 2002);
    assert_eq!(message(&err), "vault sealed: negative key");
}

/// A producer module that exports a `#[deprecated]` function. The generated
/// thunk must still *call* the deprecated function, so it carries an
/// `#[allow(deprecated)]` of its own; this module compiling under the
/// workspace's `-D warnings` policy is the proof.
#[weaveffi::module]
pub mod legacy {
    /// The modern entry point.
    #[weaveffi::export]
    pub fn add_one(value: i64) -> i64 {
        value + 1
    }

    /// A retired entry point kept for one more release.
    #[deprecated(note = "use add_one")]
    #[weaveffi::export]
    pub fn bump(value: i64) -> i64 {
        value + 1
    }
}

/// A producer module built around an interface: an opaque object with
/// constructors, methods, statics, and a destructor.
#[weaveffi::module]
pub mod counters {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    /// The counters error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CounterError {
        /// start value out of range
        OutOfRange = 1,
    }

    impl std::fmt::Display for CounterError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("step must be positive")
        }
    }

    /// A monotonic counter, exported as an interface.
    #[weaveffi::interface]
    pub struct Counter {
        value: AtomicI64,
        step: i64,
    }

    impl Counter {
        /// Create a counter starting at `start`, stepping by 1.
        pub fn new(start: i64) -> Self {
            Self {
                value: AtomicI64::new(start),
                step: 1,
            }
        }

        /// Create a counter with a custom step, rejecting non-positive steps.
        pub fn with_step(start: i64, step: i64) -> Result<Counter, CounterError> {
            if step <= 0 {
                return Err(CounterError::OutOfRange);
            }
            Ok(Counter {
                value: AtomicI64::new(start),
                step,
            })
        }

        /// Advance the counter and return the new value.
        pub fn increment(&self) -> i64 {
            self.value.fetch_add(self.step, Ordering::Relaxed) + self.step
        }

        /// Read the current value without advancing.
        pub fn value(&self) -> i64 {
            self.value.load(Ordering::Relaxed)
        }

        /// Render the value with a prefix (string arg + string return).
        pub fn label(&self, prefix: &str) -> String {
            format!("{prefix}{}", self.value())
        }

        /// Clone the counter at its current value (interface return).
        pub fn snapshot(&self) -> Counter {
            Counter {
                value: AtomicI64::new(self.value()),
                step: self.step,
            }
        }

        /// Return a new reference to this same counter (`Arc<Self>` receiver
        /// and return).
        pub fn share(self: Arc<Self>) -> Arc<Self> {
            self
        }

        /// Return the counter with the larger value, or none if both are
        /// below `floor` (optional object parameter and return).
        pub fn larger(&self, other: Option<&Counter>, floor: i64) -> Option<Arc<Counter>> {
            let mine = self.value();
            let theirs = other.map(Counter::value);
            match theirs {
                Some(t) if t >= mine && t >= floor => Some(Arc::new(Counter {
                    value: AtomicI64::new(t),
                    step: 1,
                })),
                _ if mine >= floor => Some(Arc::new(Counter::new(mine))),
                _ => None,
            }
        }

        /// Yield `n` fresh counters lazily (interface elements in an iterator).
        pub fn fan_out(&self, n: i32) -> weaveffi::Iter<Arc<Counter>> {
            let base = self.value();
            weaveffi::Iter::new((0..n as i64).map(move |i| Arc::new(Counter::new(base + i))))
        }

        /// Read the value asynchronously (an async method retains `self`).
        pub async fn value_later(&self) -> i64 {
            self.value()
        }

        /// Return the same counter asynchronously (async object result).
        pub async fn snapshot_later(self: Arc<Self>) -> Arc<Counter> {
            self
        }

        /// Panic on purpose, proving panics surface as errors, not aborts.
        pub fn explode(&self) {
            panic!("counter exploded");
        }

        /// The default start value (a static under the interface namespace).
        pub fn default_start() -> i64 {
            0
        }

        // A private helper: not exported.
        #[allow(dead_code)]
        fn internal(&self) -> i64 {
            -1
        }
    }

    /// A free function taking the interface by reference.
    #[weaveffi::export]
    pub fn read_twice(counter: &Counter) -> i64 {
        counter.value() * 2
    }
}

#[test]
fn interface_constructor_methods_destroy() {
    let mut err = ok_err();
    unsafe {
        let c = counters::runtime_counters_Counter_new(10, &mut err);
        assert_eq!(err.code, 0);
        assert!(!c.is_null());

        assert_eq!(
            counters::runtime_counters_Counter_increment(c, &mut err),
            11
        );
        assert_eq!(
            counters::runtime_counters_Counter_increment(c, &mut err),
            12
        );
        assert_eq!(counters::runtime_counters_Counter_value(c, &mut err), 12);
        assert_eq!(err.code, 0);

        let prefix = "n=";
        let mut len = 0usize;
        let label = counters::runtime_counters_Counter_label(
            c,
            prefix.as_ptr(),
            prefix.len(),
            &mut len,
            &mut err,
        );
        assert_eq!(take_string(label, len), "n=12");

        counters::runtime_counters_Counter_destroy(c);
    }
}

#[test]
fn interface_fallible_constructor() {
    let mut err = ok_err();
    unsafe {
        let ok = counters::runtime_counters_Counter_with_step(0, 5, &mut err);
        assert_eq!(err.code, 0);
        assert!(!ok.is_null());
        assert_eq!(
            counters::runtime_counters_Counter_increment(ok, &mut err),
            5
        );
        counters::runtime_counters_Counter_destroy(ok);

        let bad = counters::runtime_counters_Counter_with_step(0, 0, &mut err);
        assert!(bad.is_null());
        assert_eq!(err.code, 1, "domain code from the #[weaveffi::error] enum");
        assert_eq!(message(&err), "step must be positive");
    }
}

#[test]
fn interface_returning_method_and_static() {
    let mut err = ok_err();
    unsafe {
        assert_eq!(
            counters::runtime_counters_Counter_default_start(&mut err),
            0
        );

        let c = counters::runtime_counters_Counter_new(3, &mut err);
        let snap = counters::runtime_counters_Counter_snapshot(c, &mut err);
        assert!(!snap.is_null());
        counters::runtime_counters_Counter_increment(c, &mut err);
        assert_eq!(counters::runtime_counters_Counter_value(c, &mut err), 4);
        assert_eq!(
            counters::runtime_counters_Counter_value(snap, &mut err),
            3,
            "the snapshot is an independent object"
        );
        counters::runtime_counters_Counter_destroy(snap);
        counters::runtime_counters_Counter_destroy(c);
    }
}

#[test]
fn interface_as_free_function_parameter() {
    let mut err = ok_err();
    unsafe {
        let c = counters::runtime_counters_Counter_new(21, &mut err);
        assert_eq!(counters::runtime_counters_read_twice(c, &mut err), 42);
        counters::runtime_counters_Counter_destroy(c);
    }
}

#[test]
fn arc_self_receiver_and_optional_objects() {
    let mut err = ok_err();
    unsafe {
        let c = counters::runtime_counters_Counter_new(10, &mut err);
        let shared = counters::runtime_counters_Counter_share(c, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(shared, c, "`self: Arc<Self>` returns the same object");
        counters::runtime_counters_Counter_destroy(shared);

        let other = counters::runtime_counters_Counter_new(20, &mut err);
        let bigger = counters::runtime_counters_Counter_larger(c, other, 0, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(
            counters::runtime_counters_Counter_value(bigger, &mut err),
            20
        );
        counters::runtime_counters_Counter_destroy(bigger);

        let mine = counters::runtime_counters_Counter_larger(c, std::ptr::null(), 0, &mut err);
        assert_eq!(counters::runtime_counters_Counter_value(mine, &mut err), 10);
        counters::runtime_counters_Counter_destroy(mine);

        let none = counters::runtime_counters_Counter_larger(c, other, 100, &mut err);
        assert!(none.is_null());
        assert_eq!(err.code, 0, "a null optional object return is not an error");

        counters::runtime_counters_Counter_destroy(other);
        counters::runtime_counters_Counter_destroy(c);
    }
}

#[test]
fn iterator_of_objects() {
    let mut err = ok_err();
    let mut values = Vec::new();
    unsafe {
        let c = counters::runtime_counters_Counter_new(5, &mut err);
        let iter = counters::runtime_counters_Counter_fan_out(c, 3, &mut err);
        assert_eq!(err.code, 0);
        loop {
            let mut item: *mut counters::Counter = std::ptr::null_mut();
            if counters::runtime_counters_Counter_FanOutIterator_next(iter, &mut item, &mut err)
                == 0
            {
                break;
            }
            values.push(counters::runtime_counters_Counter_value(item, &mut err));
            counters::runtime_counters_Counter_destroy(item);
        }
        counters::runtime_counters_Counter_FanOutIterator_destroy(iter);
        counters::runtime_counters_Counter_destroy(c);
    }
    assert_eq!(values, vec![5, 6, 7]);
}

#[test]
fn async_methods_retain_the_receiver() {
    extern "C" fn on_obj(ctx: *mut c_void, err: *mut FfiError, result: *mut counters::Counter) {
        let mut e = FfiError::default();
        let v = unsafe {
            let v = counters::runtime_counters_Counter_value(result, &mut e);
            counters::runtime_counters_Counter_destroy(result);
            v
        };
        send(ctx, err, v);
    }

    let mut err = ok_err();
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i64>(tx);

    unsafe {
        let c = counters::runtime_counters_Counter_new(8, &mut err);
        counters::runtime_counters_Counter_value_later(c, on_i64, ctx);
        counters::runtime_counters_Counter_snapshot_later(c, on_obj, ctx);
        // Releasing the consumer's reference while calls are in flight is
        // safe: each launcher retained its own.
        counters::runtime_counters_Counter_destroy(c);
    }
    let mut got = vec![
        rx.recv_timeout(WAIT).unwrap().2,
        rx.recv_timeout(WAIT).unwrap().2,
    ];
    got.sort_unstable();
    assert_eq!(got, vec![8, 8]);

    // A null receiver still completes (with a marshalling error).
    unsafe { counters::runtime_counters_Counter_value_later(std::ptr::null(), on_i64, ctx) };
    let (code, _, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!((code, result), (abi::MARSHAL_ERROR_CODE, 0));
    drop_ctx::<i64>(ctx);
}

#[test]
fn interface_null_self_reports_error() {
    let mut err = ok_err();
    let r = unsafe { counters::runtime_counters_Counter_value(std::ptr::null(), &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
}

#[test]
fn producer_panic_reports_panic_code() {
    let mut err = ok_err();
    unsafe {
        let c = counters::runtime_counters_Counter_new(0, &mut err);

        counters::runtime_counters_Counter_explode(c, &mut err);
        assert_eq!(err.code, abi::PANIC_ERROR_CODE);
        assert!(message(&err).contains("counter exploded"));

        // The object is still usable and the error slot resets on the next call.
        assert_eq!(counters::runtime_counters_Counter_value(c, &mut err), 0);
        assert_eq!(err.code, 0);
        counters::runtime_counters_Counter_destroy(c);
    }
}

#[test]
fn deprecated_export_thunk_compiles_and_runs() {
    let mut err = ok_err();
    assert_eq!(unsafe { legacy::runtime_legacy_add_one(41, &mut err) }, 42);
    assert_eq!(err.code, 0);

    #[allow(deprecated)]
    let bumped = unsafe { legacy::runtime_legacy_bump(41, &mut err) };
    assert_eq!(bumped, 42);
    assert_eq!(err.code, 0);
}

/// A producer module whose error domain carries structured payload fields:
/// the variant's named fields travel through the error slot's
/// `payload_ptr`/`payload_len` serialized in the value-buffer format.
#[weaveffi::module]
pub mod quota {
    /// The quota error domain. `Exceeded` carries a structured payload.
    #[weaveffi::error]
    #[derive(Debug)]
    #[repr(i32)]
    pub enum QuotaError {
        /// quota exceeded
        Exceeded {
            /// The configured limit.
            limit: i64,
            /// The amount actually used.
            used: i64,
        } = 3001,
        /// quota service unavailable
        Unavailable = 3002,
    }

    impl std::fmt::Display for QuotaError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Exceeded { limit, used } => write!(f, "used {used} of {limit}"),
                Self::Unavailable => f.write_str("quota service unavailable"),
            }
        }
    }

    /// Consume `amount` units against a limit of 100.
    #[weaveffi::export]
    pub fn consume(amount: i64) -> Result<i64, QuotaError> {
        match amount {
            a if a < 0 => Err(QuotaError::Unavailable),
            a if a > 100 => Err(QuotaError::Exceeded {
                limit: 100,
                used: a,
            }),
            a => Ok(100 - a),
        }
    }
}

#[test]
fn error_payload_fields_cross_the_abi() {
    let mut err = ok_err();

    // Success leaves the payload slots empty.
    assert_eq!(unsafe { quota::runtime_quota_consume(30, &mut err) }, 70);
    assert_eq!(err.code, 0);
    assert!(err.payload_ptr.is_null());

    // A payload-carrying variant serializes its fields in declaration order.
    let r = unsafe { quota::runtime_quota_consume(250, &mut err) };
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, 3001);
    assert_eq!(message(&err), "used 250 of 100");
    assert!(!err.payload_ptr.is_null());
    let payload = unsafe { std::slice::from_raw_parts(err.payload_ptr, err.payload_len) };
    let mut reader = abi::BufferReader::new(payload);
    assert_eq!(reader.read_i64().unwrap(), 100, "limit field");
    assert_eq!(reader.read_i64().unwrap(), 250, "used field");
    reader.expect_end().unwrap();
    unsafe { runtime_error_clear(&mut err) };
    assert!(err.payload_ptr.is_null(), "clear releases the payload");

    // A unit variant reports code and message with no payload.
    let r = unsafe { quota::runtime_quota_consume(-1, &mut err) };
    assert_eq!(r, 0);
    assert_eq!(err.code, 3002);
    assert!(err.payload_ptr.is_null());
}

/// A consumer-side `Source`: returns of every family, allocated with the
/// exported `{prefix}_alloc`, and typed failures with a payload.
mod consumer_source {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe fn give(bytes: &[u8], out_ptr: *mut *mut u8, out_len: *mut usize) {
        let run = super::runtime_alloc(bytes.len());
        unsafe {
            if !bytes.is_empty() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), run, bytes.len());
            }
            *out_ptr = run;
            *out_len = bytes.len();
        }
    }

    unsafe extern "C" fn name(
        _ctx: *mut c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        _err: *mut FfiError,
    ) {
        unsafe { give(b"src", out_ptr, out_len) };
    }

    unsafe extern "C" fn blob(
        _ctx: *mut c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        _err: *mut FfiError,
    ) {
        unsafe { give(&[1, 2], out_ptr, out_len) };
    }

    unsafe extern "C" fn card(
        _ctx: *mut c_void,
        key_ptr: *const u8,
        key_len: usize,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        _err: *mut FfiError,
    ) {
        let key = unsafe { abi::lift_string(key_ptr, key_len) }.unwrap();
        let card = rich::Card {
            name: format!("card-{key}"),
            tags: vec!["t".into()],
        };
        unsafe { give(&abi::encode_value(&card), out_ptr, out_len) };
    }

    unsafe extern "C" fn token(_ctx: *mut c_void, _err: *mut FfiError) -> *mut rich::Token {
        let mut err = FfiError::default();
        unsafe { rich::runtime_rich_Token_new(5, &mut err) }
    }

    unsafe extern "C" fn maybe_token(_ctx: *mut c_void, _err: *mut FfiError) -> *mut rich::Token {
        std::ptr::null_mut()
    }

    unsafe extern "C" fn lookup(
        _ctx: *mut c_void,
        key_ptr: *const u8,
        key_len: usize,
        out_err: *mut FfiError,
    ) -> i64 {
        let key = unsafe { abi::lift_string(key_ptr, key_len) }.unwrap();
        let fail = |code: i32, payload: Option<&str>| unsafe {
            let msg = std::ffi::CString::new(format!("failed {key}")).unwrap();
            super::runtime_error_set(out_err, code, msg.as_ptr());
            if let Some(p) = payload {
                let fields = abi::encode_value(&p.to_string());
                super::runtime_error_set_payload(out_err, fields.as_ptr(), fields.len());
            }
        };
        match key.as_str() {
            "missing" => fail(1, Some("missing")),
            "busy" => fail(2, None),
            "bogus" => fail(99, None),
            "trap" => fail(-1, None),
            _ => return 42,
        }
        0
    }

    pub static FREED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn free(_ctx: *mut c_void) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }

    pub static VTABLE: rich::runtime_rich_Source_vtable = rich::runtime_rich_Source_vtable {
        header: abi::VtableHeader {
            size: std::mem::size_of::<rich::runtime_rich_Source_vtable>() as u32,
            flags: 0,
            free,
        },
        name,
        blob,
        card,
        token,
        maybe_token,
        lookup,
    };
}

#[test]
fn callback_returns_of_every_family_are_adopted() {
    let mut err = ok_err();
    let mut len = 0usize;
    let ptr = unsafe {
        rich::runtime_rich_describe(
            std::ptr::null_mut(),
            &consumer_source::VTABLE,
            &mut len,
            &mut err,
        )
    };
    assert_eq!(err.code, 0, "{}", message(&err));
    assert_eq!(take_string(ptr, len), r#"src [1, 2] card-k:["t"] 5 false"#);
}

#[test]
fn throwing_callbacks_report_typed_domain_errors() {
    let lookup = |key: &str| {
        let mut err = ok_err();
        let mut len = 0usize;
        let ptr = unsafe {
            rich::runtime_rich_lookup_via(
                std::ptr::null_mut(),
                &consumer_source::VTABLE,
                key.as_ptr(),
                key.len(),
                &mut len,
                &mut err,
            )
        };
        assert_eq!(err.code, 0);
        take_string(ptr, len)
    };
    assert_eq!(lookup("a"), "ok 42");
    assert_eq!(
        lookup("missing"),
        "missing missing",
        "code 1 with its payload"
    );
    assert_eq!(lookup("busy"), "busy");
    // An undeclared positive code, or any negative one, is a foreign failure.
    assert_eq!(lookup("bogus"), "foreign -4: failed bogus");
    assert_eq!(lookup("trap"), "foreign -4: failed trap");
}

#[test]
fn optional_callback_parameters_accept_null() {
    let label = "x";
    let mut err = ok_err();
    let mut len = 0usize;
    let none = unsafe {
        rich::runtime_rich_has_source(
            std::ptr::null_mut(),
            std::ptr::null(),
            label.as_ptr(),
            label.len(),
            &mut len,
            &mut err,
        )
    };
    assert_eq!(take_string(none, len), "x:false");
    let some = unsafe {
        rich::runtime_rich_has_source(
            std::ptr::null_mut(),
            &consumer_source::VTABLE,
            label.as_ptr(),
            label.len(),
            &mut len,
            &mut err,
        )
    };
    assert_eq!(take_string(some, len), "x:true");
    assert_eq!(unsafe { rich::runtime_rich_always(&mut err) }, 1);
}

#[test]
fn many_concurrent_launches_share_a_few_executor_threads() {
    extern "C" fn on_tag(ctx: *mut c_void, err: *mut FfiError, ptr: *const u8, len: usize) {
        on_string(ctx, err, ptr, len);
    }
    const CALLS: usize = 64;
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<String>(tx);
    for _ in 0..CALLS {
        unsafe { tasks::runtime_tasks_thread_tag(on_tag, ctx) };
    }
    let mut threads = std::collections::BTreeSet::new();
    for _ in 0..CALLS {
        let (code, _, tag) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(code, 0);
        assert!(tag.contains("weaveffi-async"), "{tag}");
        threads.insert(tag);
    }
    drop_ctx::<String>(ctx);
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .max(2);
    assert!(
        threads.len() <= workers,
        "{} threads ran {CALLS} calls with {workers} workers",
        threads.len()
    );
}
