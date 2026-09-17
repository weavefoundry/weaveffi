//! End-to-end runtime tests for the `#[weaveffi::module]` expansion.
//!
//! Each test defines a module with the macro, then calls the generated
//! `#[no_mangle] extern "C"` thunks directly (by their Rust path) and checks
//! that arguments lift, results lower, and errors flow through `out_err` the
//! way the C ABI promises. This is the executable proof that the generated glue
//! matches the calling convention every language binding expects.

#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use std::sync::Arc;
use weaveffi::abi::{self, c_ptr_to_string, free_string, string_to_c_ptr, weaveffi_error};

#[weaveffi::module]
pub mod demo {
    /// The demo module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum DemoError {
        /// division by zero
        DivisionByZero = 100,
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

    /// Borrow a string slice and report its length.
    #[weaveffi::export]
    pub fn str_len(text: &str) -> i32 {
        text.chars().count() as i32
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
    // A struct declared in a *sibling* top-level module. The macro expands each
    // module in isolation, so this exercises cross-module type resolution: the
    // thunk must accept/return the producer's real `Crate` type as an opaque
    // pointer without the per-module expansion rejecting it as unknown.
    use super::warehouse::Crate;

    /// Read a sibling-module record's id (struct parameter by value).
    #[weaveffi::export]
    pub fn crate_id(item: Crate) -> i64 {
        item.id
    }

    /// Return a relabeled copy (sibling-module struct in and out).
    #[weaveffi::export]
    pub fn relabel(item: Crate, label: String) -> Crate {
        Crate { id: item.id, label }
    }
}

/// Stands in for a `panic = "abort"` build, where a consumer callback failure
/// can't unwind: the producer records it with `defer_foreign_error` and keeps
/// running, and the thunk must report the recorded failure in place of the
/// producer's result.
#[weaveffi::module]
pub mod deferred {
    /// Record a foreign failure, then return normally with a value the
    /// caller must never see.
    #[weaveffi::export]
    pub fn sync_then_fail(fail: bool) -> i32 {
        if fail {
            weaveffi::abi::defer_foreign_error(weaveffi::abi::ForeignError {
                code: weaveffi::abi::FOREIGN_ERROR_CODE,
                message: "consumer said no".to_string(),
            });
        }
        77
    }

    /// The async twin of `sync_then_fail`.
    #[weaveffi::export]
    pub async fn later_then_fail(fail: bool) -> i32 {
        if fail {
            weaveffi::abi::defer_foreign_error(weaveffi::abi::ForeignError {
                code: weaveffi::abi::FOREIGN_ERROR_CODE,
                message: "consumer said no, later".to_string(),
            });
        }
        88
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
}

#[weaveffi::module]
pub mod bus {
    use std::sync::{Arc, Mutex, PoisonError};

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
        fn on_message(&self, text: String, weight: i32, envelope: &Envelope) -> i64;
        /// Ask the subscriber how urgent it considers `weight`.
        fn classify(&self, weight: i32) -> Priority;
        /// Inspect a shared object without retaining it.
        fn on_ticker(&self, ticker: Arc<Ticker>, alt: Option<Arc<Ticker>>) -> bool;
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

        /// Publish to every subscriber, returning the sum of their totals.
        ///
        /// A subscriber failure unwinds through this frame like a panic, so
        /// the subscriber list is snapshotted and the lock released before
        /// any callback runs (the same discipline a panic-safe producer uses).
        pub fn publish(&self, text: String, weight: i32) -> i64 {
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
                .map(|s| s.on_message(text.clone(), weight, &env))
                .sum()
        }

        /// Publish asynchronously.
        pub async fn publish_later(&self, text: String, weight: i32) -> i64 {
            self.publish(text, weight)
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
    pub fn classify_once(subscriber: Arc<dyn Subscriber>, weight: i32) -> Priority {
        subscriber.classify(weight)
    }

    /// Hand the subscriber a ticker object.
    #[weaveffi::export]
    pub fn tick(subscriber: &Arc<dyn Subscriber>, value: i64) -> bool {
        let ticker = Arc::new(Ticker::new(value));
        subscriber.on_ticker(ticker.clone(), None)
            && subscriber.on_ticker(ticker.clone(), Some(ticker))
    }
}

#[weaveffi::module]
pub mod tasks {
    /// The task module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// arithmetic overflow
        Overflow = 1,
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

    /// Add two integers asynchronously, failing on overflow.
    #[weaveffi::export]
    pub async fn checked_add(a: i32, b: i32) -> Result<i32, TaskError> {
        a.checked_add(b).ok_or(TaskError::Overflow)
    }
}

fn ok_err() -> weaveffi_error {
    weaveffi_error::default()
}

/// Decode a buffered return `(ptr, out_len)` into an owned value and release
/// the producer-allocated buffer, mirroring what every generated binding does.
fn decode_ret<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
    assert!(
        !ptr.is_null(),
        "buffered return must not be null on success"
    );
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let value = abi::decode_value(bytes).expect("well-formed value buffer");
    abi::free_bytes(ptr as *mut u8, len);
    value
}

#[test]
fn scalar_call_sets_ok() {
    let mut err = ok_err();
    let r = demo::weaveffi_demo_add(2, 40, &mut err);
    assert_eq!(r, 42);
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());
}

#[test]
fn fallible_ok_and_err_paths() {
    let mut err = ok_err();
    assert_eq!(demo::weaveffi_demo_checked_div(10, 2, &mut err), 5);
    assert_eq!(err.code, 0);

    let r = demo::weaveffi_demo_checked_div(1, 0, &mut err);
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(
        err.code, 100,
        "domain code from the #[weaveffi::error] enum"
    );
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "division by zero");
    abi::error_clear(&mut err);
}

#[test]
fn owned_string_roundtrip() {
    let mut err = ok_err();
    let input = string_to_c_ptr("alice");
    let out = demo::weaveffi_demo_greet(input, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(c_ptr_to_string(out).unwrap(), "hi alice");
    free_string(out);
    free_string(input);
}

#[test]
fn borrowed_str_param() {
    let mut err = ok_err();
    let input = string_to_c_ptr("héllo");
    assert_eq!(demo::weaveffi_demo_str_len(input, &mut err), 5);
    free_string(input);
}

#[test]
fn optional_string_return_is_buffered() {
    let mut err = ok_err();
    let mut out_len: usize = 0;
    let some = demo::weaveffi_demo_maybe_name(true, &mut out_len, &mut err);
    assert_eq!(
        decode_ret::<Option<String>>(some, out_len),
        Some("present".to_string())
    );

    let none = demo::weaveffi_demo_maybe_name(false, &mut out_len, &mut err);
    assert_eq!(decode_ret::<Option<String>>(none, out_len), None);
}

#[test]
fn scalar_list_param_is_buffered() {
    let mut err = ok_err();
    let xs = abi::encode_value(&vec![3i32, 4, 5]);
    let total = demo::weaveffi_demo_sum(xs.as_ptr(), xs.len(), &mut err);
    assert_eq!(total, 12);
}

#[test]
fn string_list_param_is_buffered() {
    let mut err = ok_err();
    let parts = abi::encode_value(&vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    let out = demo::weaveffi_demo_join(parts.as_ptr(), parts.len(), &mut err);
    assert_eq!(c_ptr_to_string(out).unwrap(), "a,b,c");
    free_string(out);
}

#[test]
fn malformed_buffer_param_reports_error() {
    let mut err = ok_err();
    // A truncated encoding (length prefix with no elements) must be rejected
    // through `out_err`, never decoded partially.
    let bad = [9u8, 0, 0, 0];
    let total = demo::weaveffi_demo_sum(bad.as_ptr(), bad.len(), &mut err);
    assert_eq!(total, 0, "error path returns the zero sentinel");
    assert_ne!(err.code, 0);
    abi::error_clear(&mut err);
}

#[test]
fn byte_buffer_param() {
    let mut err = ok_err();
    let data = [1u8, 2, 3, 4, 5];
    assert_eq!(
        demo::weaveffi_demo_byte_count(data.as_ptr(), data.len(), &mut err),
        5
    );
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
    let back: demo::Point = abi::decode_value(&bytes).expect("round-trip");
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
    let x = demo::weaveffi_demo_point_x(bytes.as_ptr(), bytes.len(), &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(x, 41);
}

#[test]
fn struct_return_is_buffered() {
    let mut err = ok_err();
    let mut out_len: usize = 0;
    let ptr = demo::weaveffi_demo_make_point(99, &mut out_len, &mut err);
    assert_eq!(err.code, 0);
    let p: demo::Point = decode_ret(ptr, out_len);
    assert_eq!(p.x, 99);
    assert_eq!(p.label, "origin");
    assert_eq!(p.nickname, None);
    assert_eq!(p.color, demo::Color::Green);
}

#[test]
fn cross_module_struct_param_and_return() {
    let mut err = ok_err();
    let label = string_to_c_ptr("widget");
    let mut out_len: usize = 0;
    let ptr = warehouse::weaveffi_warehouse_make_crate(7, label, &mut out_len, &mut err);
    assert_eq!(err.code, 0);
    free_string(label);
    let c: warehouse::Crate = decode_ret(ptr, out_len);
    assert_eq!(c.id, 7);
    assert_eq!(c.label, "widget");

    // `dispatch::crate_id` accepts `warehouse::Crate` as a value buffer.
    let bytes = abi::encode_value(&c);
    let id = dispatch::weaveffi_dispatch_crate_id(bytes.as_ptr(), bytes.len(), &mut err);
    assert_eq!(id, 7);
    assert_eq!(err.code, 0);

    // `dispatch::relabel` returns a fresh `warehouse::Crate` buffer decoded
    // with the same impl (same Rust type, same wire format).
    let new_label = string_to_c_ptr("gadget");
    let ptr2 = dispatch::weaveffi_dispatch_relabel(
        bytes.as_ptr(),
        bytes.len(),
        new_label,
        &mut out_len,
        &mut err,
    );
    free_string(new_label);
    let c2: warehouse::Crate = decode_ret(ptr2, out_len);
    assert_eq!(c2.id, 7);
    assert_eq!(c2.label, "gadget");
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
    let ptr =
        maps::weaveffi_maps_double_scores(bytes.as_ptr(), bytes.len(), &mut out_len, &mut err);
    assert_eq!(err.code, 0);
    let doubled: BTreeMap<String, i32> = decode_ret(ptr, out_len);
    assert_eq!(doubled.get("a"), Some(&4));
    assert_eq!(doubled.get("b"), Some(&2));
}

#[test]
fn map_param_scalar_return() {
    use std::collections::BTreeMap;
    let mut err = ok_err();
    let mut scores = BTreeMap::new();
    scores.insert("a".to_string(), 10i32);
    scores.insert("b".to_string(), 32i32);
    let bytes = abi::encode_value(&scores);
    let total = maps::weaveffi_maps_total(bytes.as_ptr(), bytes.len(), &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(total, 42);
}

#[test]
fn widget_optional_field_round_trips() {
    let with_note = build::Widget {
        name: "bolt".to_string(),
        qty: 7,
        note: Some("aisle 4".to_string()),
    };
    let back: build::Widget = abi::decode_value(&abi::encode_value(&with_note)).unwrap();
    assert_eq!(back, with_note);

    let without_note = build::Widget {
        name: "nut".to_string(),
        qty: 1,
        note: None,
    };
    let back: build::Widget = abi::decode_value(&abi::encode_value(&without_note)).unwrap();
    assert_eq!(back, without_note);
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
    let back: geom::Shape = abi::decode_value(&bytes).unwrap();
    assert_eq!(back, labeled);

    // An out-of-range tag is a decode error, not a silent default.
    assert!(abi::decode_value::<geom::Shape>(&[9, 0, 0, 0]).is_err());
}

#[test]
fn rich_enum_param_is_buffered() {
    let mut err = ok_err();
    let circle = abi::encode_value(&geom::Shape::Circle { radius: 2.5 });
    let d = geom::weaveffi_geom_describe(circle.as_ptr(), circle.len(), &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(c_ptr_to_string(d).unwrap(), "circle(2.5)");
    free_string(d);
}

#[test]
fn iterator_string_elements() {
    let mut err = ok_err();
    let iter = stream::weaveffi_stream_greetings(3, &mut err);
    assert_eq!(err.code, 0);
    assert!(!iter.is_null());

    let mut got = Vec::new();
    loop {
        let mut item: *const c_char = std::ptr::null();
        let has = stream::weaveffi_stream_GreetingsIterator_next(iter, &mut item, &mut err);
        assert_eq!(err.code, 0);
        if has == 0 {
            break;
        }
        got.push(c_ptr_to_string(item).unwrap());
        free_string(item);
    }
    stream::weaveffi_stream_GreetingsIterator_destroy(iter);
    assert_eq!(got, vec!["hi 0", "hi 1", "hi 2"]);
}

#[test]
fn iterator_scalar_elements() {
    let mut err = ok_err();
    let iter = stream::weaveffi_stream_squares(4, &mut err);
    let mut got = Vec::new();
    loop {
        let mut item: i32 = 0;
        if stream::weaveffi_stream_SquaresIterator_next(iter, &mut item, &mut err) == 0 {
            break;
        }
        got.push(item);
    }
    stream::weaveffi_stream_SquaresIterator_destroy(iter);
    assert_eq!(got, vec![0, 1, 4, 9]);
}

/// A consumer-side `Subscriber` implementation: the context is a heap-allocated
/// `SubState`, the vtable is a process-wide static, exactly as a generated
/// binding would do it.
mod consumer_subscriber {
    use super::*;
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    pub struct SubState {
        pub total: AtomicI64,
        pub fail_at: i32,
        pub last_topic: std::sync::Mutex<Option<String>>,
        pub freed: Arc<AtomicUsize>,
    }

    unsafe extern "C" fn on_message(
        ctx: *mut c_void,
        text: *const c_char,
        weight: i32,
        envelope_ptr: *const u8,
        envelope_len: usize,
        out_err: *mut weaveffi_error,
    ) -> i64 {
        let state = &*(ctx as *const SubState);
        let text = c_ptr_to_string(text).unwrap();
        let env: bus::Envelope =
            abi::decode_value(std::slice::from_raw_parts(envelope_ptr, envelope_len)).unwrap();
        *state.last_topic.lock().unwrap() = env.topic.clone();
        if weight == state.fail_at {
            abi::error_set(
                out_err,
                abi::FOREIGN_ERROR_CODE,
                &format!("subscriber rejected {text}"),
            );
            return 0;
        }
        abi::error_set_ok(out_err);
        state.total.fetch_add(weight as i64, Ordering::Relaxed) + weight as i64
    }

    unsafe extern "C" fn classify(
        _ctx: *mut c_void,
        weight: i32,
        out_err: *mut weaveffi_error,
    ) -> i32 {
        abi::error_set_ok(out_err);
        if weight > 5 {
            1
        } else {
            0
        }
    }

    unsafe extern "C" fn on_ticker(
        _ctx: *mut c_void,
        ticker: *mut bus::Ticker,
        alt: *mut bus::Ticker,
        out_err: *mut weaveffi_error,
    ) -> bool {
        abi::error_set_ok(out_err);
        // Object arguments transfer one strong reference: the consumer adopts
        // each non-null pointer and owes exactly one `_destroy`.
        let mut err = weaveffi_error::default();
        let v = bus::weaveffi_bus_Ticker_value(ticker, &mut err);
        bus::weaveffi_bus_Ticker_destroy(ticker);
        let alt_ok = if alt.is_null() {
            true
        } else {
            let same = bus::weaveffi_bus_Ticker_value(alt, &mut err) == v;
            bus::weaveffi_bus_Ticker_destroy(alt);
            same
        };
        v == 42 && alt_ok
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        let state = Box::from_raw(ctx as *mut SubState);
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    pub static VTABLE: bus::weaveffi_bus_Subscriber_vtable = bus::weaveffi_bus_Subscriber_vtable {
        on_message,
        classify,
        on_ticker,
        free,
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

#[test]
fn callback_interface_sync_paths() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::sync::atomic::AtomicUsize;
    let freed = Arc::new(AtomicUsize::new(0));
    use std::sync::atomic::Ordering;

    let mut err = ok_err();

    // Direct-family return through a non-retained callback: `free` runs as soon
    // as the thunk drops its `Arc<dyn Subscriber>`.
    let ctx = new_ctx(-1, &freed);
    assert_eq!(
        bus::weaveffi_bus_classify_once(ctx, &VTABLE, 9, &mut err),
        1
    );
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 1);

    // Objects flow producer -> consumer as borrowed pointers the consumer may
    // clone; a `&Arc<dyn Trait>` spelling lends the lifted callback.
    let ctx = new_ctx(-1, &freed);
    assert!(bus::weaveffi_bus_tick(ctx, &VTABLE, 42, &mut err));
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 2);

    // A null vtable is a marshalling error, not a crash.
    let r = bus::weaveffi_bus_classify_once(std::ptr::null_mut(), std::ptr::null(), 1, &mut err);
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    abi::error_clear(&mut err);
}

#[test]
fn callback_interface_retained_and_foreign_error() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::sync::atomic::AtomicUsize;
    let freed = Arc::new(AtomicUsize::new(0));
    use std::sync::atomic::Ordering;

    let mut err = ok_err();

    let b = bus::weaveffi_bus_Bus_new(&mut err);
    assert!(!b.is_null());
    bus::weaveffi_bus_Bus_subscribe(b, new_ctx(7, &freed), &VTABLE, &mut err);
    bus::weaveffi_bus_Bus_subscribe(b, new_ctx(-1, &freed), &VTABLE, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 0, "retained by the bus");

    let text = string_to_c_ptr("hi");
    assert_eq!(bus::weaveffi_bus_Bus_publish(b, text, 3, &mut err), 6);
    assert_eq!(err.code, 0);
    assert_eq!(bus::weaveffi_bus_Bus_publish(b, text, 5, &mut err), 16);

    // The first subscriber fails on weight 7: the producer call is aborted and
    // the consumer's own message comes back with FOREIGN_ERROR_CODE.
    let r = bus::weaveffi_bus_Bus_publish(b, text, 7, &mut err);
    assert_eq!(r, 0);
    assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(
        c_ptr_to_string(err.message).unwrap(),
        "subscriber rejected hi"
    );
    abi::error_clear(&mut err);

    // The bus is still usable afterwards.
    assert_eq!(bus::weaveffi_bus_Bus_publish(b, text, 1, &mut err), 18);
    assert_eq!(err.code, 0);

    bus::weaveffi_bus_Bus_clear(b, &mut err);
    assert_eq!(freed.load(Ordering::SeqCst), 2, "free runs once each");
    free_string(text);
    bus::weaveffi_bus_Bus_destroy(b);
}

#[test]
fn callback_interface_from_async_method() {
    use consumer_subscriber::{new_ctx, VTABLE};
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (i32, String, i64);
    extern "C" fn cb(ctx: *mut c_void, err: *mut weaveffi_error, result: i64) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let (code, msg) = if err.is_null() {
            (0, String::new())
        } else {
            let e = unsafe { &*err };
            let out = (e.code, c_ptr_to_string(e.message).unwrap_or_default());
            abi::error_free(err);
            out
        };
        tx.send((code, msg, result)).unwrap();
    }

    let mut err = ok_err();
    let b = bus::weaveffi_bus_Bus_new(&mut err);
    let freed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    bus::weaveffi_bus_Bus_subscribe(b, new_ctx(4, &freed), &VTABLE, &mut err);

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));
    let text = string_to_c_ptr("async");

    bus::weaveffi_bus_Bus_publish_later_async(b, text, 2, cb, tx_ptr as *mut c_void);
    let (code, _, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(code, 0);
    assert_eq!(result, 2);

    // A foreign failure inside the future is delivered through the callback.
    bus::weaveffi_bus_Bus_publish_later_async(b, text, 4, cb, tx_ptr as *mut c_void);
    let (code, msg, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(msg, "subscriber rejected async");
    assert_eq!(result, 0);

    // The receiver was retained across the spawn: releasing the consumer's
    // reference while a call is in flight is safe.
    bus::weaveffi_bus_Bus_publish_later_async(b, text, 1, cb, tx_ptr as *mut c_void);
    bus::weaveffi_bus_Bus_destroy(b);
    let (code, _, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(code, 0);
    assert_eq!(result, 3);

    free_string(text);
    unsafe { drop(Box::from_raw(tx_ptr)) };
}

#[test]
fn async_struct_result_completes_via_callback() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (bool, i64, String);
    // The buffered result is owned by the consumer: decode it, then release
    // the producer allocation with `free_bytes`.
    extern "C" fn cb(
        ctx: *mut c_void,
        err: *mut weaveffi_error,
        result_ptr: *const u8,
        result_len: usize,
    ) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let had_err = !err.is_null() && unsafe { (*err).code } != 0;
        if had_err {
            abi::error_free(err);
        }
        let payload = if result_ptr.is_null() {
            (had_err, 0, String::new())
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(result_ptr, result_len) };
            let r: tasks::TaskResult = abi::decode_value(bytes).expect("well-formed result");
            abi::free_bytes(result_ptr as *mut u8, result_len);
            (had_err, r.id, r.value)
        };
        tx.send(payload).unwrap();
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));
    let name = string_to_c_ptr("alpha");
    tasks::weaveffi_tasks_run_task_async(name, cb, tx_ptr as *mut c_void);
    let (had_err, id, value) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    free_string(name);
    unsafe { drop(Box::from_raw(tx_ptr)) };

    assert!(!had_err);
    assert_eq!(id, 7);
    assert_eq!(value, "done: alpha");
}

#[test]
fn async_result_ok_and_err_paths() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (bool, i32);
    extern "C" fn cb(ctx: *mut c_void, err: *mut weaveffi_error, result: i32) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let had_err = !err.is_null() && unsafe { (*err).code } != 0;
        if had_err {
            // The reported error is heap-boxed and owned by the consumer.
            abi::error_free(err);
        }
        tx.send((had_err, result)).unwrap();
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));

    tasks::weaveffi_tasks_checked_add_async(2, 3, cb, tx_ptr as *mut c_void);
    let (had_err, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!had_err);
    assert_eq!(result, 5);

    tasks::weaveffi_tasks_checked_add_async(i32::MAX, 1, cb, tx_ptr as *mut c_void);
    let (had_err, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(had_err);
    assert_eq!(result, 0);

    unsafe { drop(Box::from_raw(tx_ptr)) };
}

#[test]
fn deferred_foreign_error_replaces_a_sync_result() {
    let mut err = ok_err();
    assert_eq!(
        deferred::weaveffi_deferred_sync_then_fail(false, &mut err),
        77
    );
    assert_eq!(err.code, 0);

    let r = deferred::weaveffi_deferred_sync_then_fail(true, &mut err);
    assert_eq!(r, 0, "the producer's value is discarded for the sentinel");
    assert_eq!(err.code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "consumer said no");
    abi::error_clear(&mut err);

    assert!(
        abi::take_foreign_error().is_none(),
        "the thunk drained the recorded failure"
    );
    assert_eq!(
        deferred::weaveffi_deferred_sync_then_fail(false, &mut err),
        77
    );
    assert_eq!(err.code, 0, "a later call on the same thread is unaffected");
}

#[test]
fn deferred_foreign_error_fires_the_async_callback_once() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (i32, String, i32);
    extern "C" fn cb(ctx: *mut c_void, err: *mut weaveffi_error, result: i32) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let (code, message) = if err.is_null() {
            (0, String::new())
        } else {
            let e = unsafe { &*err };
            let out = (e.code, c_ptr_to_string(e.message).unwrap_or_default());
            abi::error_free(err);
            out
        };
        tx.send((code, message, result)).unwrap();
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));

    deferred::weaveffi_deferred_later_then_fail_async(false, cb, tx_ptr as *mut c_void);
    let (code, _, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((code, result), (0, 88));

    deferred::weaveffi_deferred_later_then_fail_async(true, cb, tx_ptr as *mut c_void);
    let (code, message, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(code, abi::FOREIGN_ERROR_CODE);
    assert_eq!(message, "consumer said no, later");
    assert_eq!(result, 0);
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the completion callback fires exactly once"
    );

    unsafe { drop(Box::from_raw(tx_ptr)) };
}

/// Exercises nested-module codegen: the inner module's symbols must carry the
/// joined `outer_inner` path, and a nested function may reference an interface
/// declared in its parent module via `super::` (the `kvstore` `stats` pattern).
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

    /// Return the same session (an `Arc<Self>`-typed parameter and return).
    #[weaveffi::export]
    pub fn share(session: Arc<Session>) -> Arc<Session> {
        session
    }

    /// The nested sub-module: its symbols use the `outer_inner` prefix.
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
    }
}

#[test]
fn nested_module_symbols_and_parent_type_reference() {
    let mut err = ok_err();
    let session = outer::weaveffi_outer_Session_open(7, &mut err);
    assert_eq!(err.code, 0);
    assert!(!session.is_null());

    // The nested function is reachable at `outer::inner::*` and its symbol
    // carries the joined module path; its record return is a value buffer.
    let mut out_len: usize = 0;
    let ptr = outer::inner::weaveffi_outer_inner_summarize(
        session,
        std::ptr::null(),
        &mut out_len,
        &mut err,
    );
    assert_eq!(err.code, 0);
    let report: outer::inner::Report = decode_ret(ptr, out_len);
    assert_eq!(report.score, 70);
    assert!(report.session.is_none());

    let ptr =
        outer::inner::weaveffi_outer_inner_summarize(session, session, &mut out_len, &mut err);
    let report: outer::inner::Report = decode_ret(ptr, out_len);
    assert_eq!(report.score, 77);

    outer::weaveffi_outer_Session_destroy(session);
}

#[test]
fn object_reference_counting() {
    let mut err = ok_err();
    let s = outer::weaveffi_outer_Session_open(3, &mut err);

    // `share` retains through `Arc<Session>` in and hands back a new strong
    // reference out; the pointer identity is the same allocation.
    let again = outer::weaveffi_outer_share(s, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(again, s, "the same object, one more reference");
    let third = outer::weaveffi_outer_Session_clone(s);
    assert_eq!(third, s);

    outer::weaveffi_outer_Session_destroy(s);
    outer::weaveffi_outer_Session_destroy(again);
    // Still alive through `third`.
    let mut out_len: usize = 0;
    let ptr = outer::inner::weaveffi_outer_inner_summarize(
        third,
        std::ptr::null(),
        &mut out_len,
        &mut err,
    );
    assert_eq!(err.code, 0);
    let report: outer::inner::Report = decode_ret(ptr, out_len);
    assert_eq!(report.score, 30);
    outer::weaveffi_outer_Session_destroy(third);
    outer::weaveffi_outer_Session_destroy(std::ptr::null_mut());
    assert!(outer::weaveffi_outer_Session_clone(std::ptr::null()).is_null());
}

#[test]
fn objects_inside_value_buffers_carry_a_reference() {
    let mut err = ok_err();
    let s = outer::weaveffi_outer_Session_open(5, &mut err);

    // `attach` retains the session inside the returned record: the buffer's
    // object token is one strong reference the consumer adopts on decode.
    let mut out_len: usize = 0;
    let ptr = outer::inner::weaveffi_outer_inner_attach(s, &mut out_len, &mut err);
    assert_eq!(err.code, 0);
    let bytes = unsafe { std::slice::from_raw_parts(ptr, out_len) }.to_vec();
    abi::free_bytes(ptr as *mut u8, out_len);
    // The consumer owns `s` and the token in `bytes`: two references.
    outer::weaveffi_outer_Session_destroy(s);

    // Sending the buffer back transfers the token's reference to the producer,
    // which returns it as the optional object result.
    let back = outer::inner::weaveffi_outer_inner_session_of(bytes.as_ptr(), bytes.len(), &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(back, s, "same allocation, still alive");
    outer::weaveffi_outer_Session_destroy(back);

    // A record with no object decodes to a null optional object return.
    let none = abi::encode_value(&outer::inner::Report {
        score: 0,
        session: None,
    });
    let back = outer::inner::weaveffi_outer_inner_session_of(none.as_ptr(), none.len(), &mut err);
    assert!(back.is_null());
    assert_eq!(err.code, 0);
}

/// A producer module whose fallible function surfaces an IDL error domain's
/// named codes through [`weaveffi::ErrorReport`].
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

    /// The producer's internal failure type. It carries payloads (which the
    /// declared domain cannot), so it maps itself onto the domain's codes with
    /// a hand-written `ErrorReport` and dynamic messages. It deliberately does
    /// not implement `Display`, which would collide with the blanket impl.
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
    assert_eq!(vault::weaveffi_vault_fetch(21, &mut err), 42);
    assert_eq!(err.code, 0);

    // `Err` carries the producer-chosen code and message verbatim.
    let r = vault::weaveffi_vault_fetch(0, &mut err);
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, 2001);
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "entry not found");
    abi::error_clear(&mut err);

    let r = vault::weaveffi_vault_fetch(-1, &mut err);
    assert_eq!(r, 0);
    assert_eq!(err.code, 2002);
    assert_eq!(
        c_ptr_to_string(err.message).unwrap(),
        "vault sealed: negative key"
    );
    abi::error_clear(&mut err);
}

/// A producer module that exports a `#[deprecated]` function. The generated
/// thunk must still *call* the deprecated function, so it has to carry an
/// `#[allow(deprecated)]` of its own; otherwise the workspace's `-D warnings`
/// policy would reject the expansion. This module compiling at all is the
/// proof.
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

        /// Return a fresh counter asynchronously (async object result).
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

    let c = counters::weaveffi_counters_Counter_new(10, &mut err);
    assert_eq!(err.code, 0);
    assert!(!c.is_null());

    assert_eq!(
        counters::weaveffi_counters_Counter_increment(c, &mut err),
        11
    );
    assert_eq!(
        counters::weaveffi_counters_Counter_increment(c, &mut err),
        12
    );
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 12);
    assert_eq!(err.code, 0);

    let prefix = string_to_c_ptr("n=");
    let label = counters::weaveffi_counters_Counter_label(c, prefix, &mut err);
    assert_eq!(c_ptr_to_string(label).unwrap(), "n=12");
    free_string(label);
    free_string(prefix);

    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn interface_fallible_constructor() {
    let mut err = ok_err();

    let ok = counters::weaveffi_counters_Counter_with_step(0, 5, &mut err);
    assert_eq!(err.code, 0);
    assert!(!ok.is_null());
    assert_eq!(
        counters::weaveffi_counters_Counter_increment(ok, &mut err),
        5
    );
    counters::weaveffi_counters_Counter_destroy(ok);

    let bad = counters::weaveffi_counters_Counter_with_step(0, 0, &mut err);
    assert!(bad.is_null());
    assert_eq!(err.code, 1, "domain code from the #[weaveffi::error] enum");
    assert_eq!(
        c_ptr_to_string(err.message).unwrap(),
        "start value out of range"
    );
    abi::error_clear(&mut err);
}

#[test]
fn interface_returning_method_and_static() {
    let mut err = ok_err();
    assert_eq!(
        counters::weaveffi_counters_Counter_default_start(&mut err),
        0
    );

    let c = counters::weaveffi_counters_Counter_new(3, &mut err);
    let snap = counters::weaveffi_counters_Counter_snapshot(c, &mut err);
    assert!(!snap.is_null());
    counters::weaveffi_counters_Counter_increment(c, &mut err);
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 4);
    assert_eq!(
        counters::weaveffi_counters_Counter_value(snap, &mut err),
        3,
        "the snapshot is an independent object"
    );
    counters::weaveffi_counters_Counter_destroy(snap);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn interface_as_free_function_parameter() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(21, &mut err);
    assert_eq!(counters::weaveffi_counters_read_twice(c, &mut err), 42);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn arc_self_receiver_and_optional_objects() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(10, &mut err);
    let shared = counters::weaveffi_counters_Counter_share(c, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(shared, c, "`self: Arc<Self>` returns the same object");
    counters::weaveffi_counters_Counter_destroy(shared);

    let other = counters::weaveffi_counters_Counter_new(20, &mut err);
    let bigger = counters::weaveffi_counters_Counter_larger(c, other, 0, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(
        counters::weaveffi_counters_Counter_value(bigger, &mut err),
        20
    );
    counters::weaveffi_counters_Counter_destroy(bigger);

    let mine = counters::weaveffi_counters_Counter_larger(c, std::ptr::null(), 0, &mut err);
    assert_eq!(
        counters::weaveffi_counters_Counter_value(mine, &mut err),
        10
    );
    counters::weaveffi_counters_Counter_destroy(mine);

    let none = counters::weaveffi_counters_Counter_larger(c, other, 100, &mut err);
    assert!(none.is_null());
    assert_eq!(err.code, 0, "a null optional object return is not an error");

    counters::weaveffi_counters_Counter_destroy(other);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn iterator_of_objects() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(5, &mut err);
    let iter = counters::weaveffi_counters_Counter_fan_out(c, 3, &mut err);
    assert_eq!(err.code, 0);
    let mut values = Vec::new();
    loop {
        let mut item: *mut counters::Counter = std::ptr::null_mut();
        if counters::weaveffi_counters_Counter_FanOutIterator_next(iter, &mut item, &mut err) == 0 {
            break;
        }
        values.push(counters::weaveffi_counters_Counter_value(item, &mut err));
        counters::weaveffi_counters_Counter_destroy(item);
    }
    counters::weaveffi_counters_Counter_FanOutIterator_destroy(iter);
    counters::weaveffi_counters_Counter_destroy(c);
    assert_eq!(values, vec![5, 6, 7]);
}

#[test]
fn async_methods_retain_the_receiver() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    extern "C" fn on_value(ctx: *mut c_void, err: *mut weaveffi_error, result: i64) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<i64>) };
        assert!(err.is_null());
        tx.send(result).unwrap();
    }
    extern "C" fn on_obj(
        ctx: *mut c_void,
        err: *mut weaveffi_error,
        result: *mut counters::Counter,
    ) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<i64>) };
        assert!(err.is_null());
        let mut e = weaveffi_error::default();
        let v = counters::weaveffi_counters_Counter_value(result, &mut e);
        counters::weaveffi_counters_Counter_destroy(result);
        tx.send(v).unwrap();
    }

    let mut err = ok_err();
    let (tx, rx) = mpsc::channel::<i64>();
    let tx_ptr = Box::into_raw(Box::new(tx));

    let c = counters::weaveffi_counters_Counter_new(8, &mut err);
    counters::weaveffi_counters_Counter_value_later_async(c, on_value, tx_ptr as *mut c_void);
    counters::weaveffi_counters_Counter_snapshot_later_async(c, on_obj, tx_ptr as *mut c_void);
    // Releasing the consumer's reference while calls are in flight is safe:
    // each launcher retained its own.
    counters::weaveffi_counters_Counter_destroy(c);
    let mut got = vec![
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
    ];
    got.sort_unstable();
    assert_eq!(got, vec![8, 8]);

    // A null receiver still completes (with a marshalling error).
    extern "C" fn on_null(ctx: *mut c_void, err: *mut weaveffi_error, result: i64) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<i64>) };
        assert!(!err.is_null());
        assert_eq!(unsafe { (*err).code }, abi::MARSHAL_ERROR_CODE);
        abi::error_free(err);
        tx.send(result).unwrap();
    }
    counters::weaveffi_counters_Counter_value_later_async(
        std::ptr::null(),
        on_null,
        tx_ptr as *mut c_void,
    );
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    unsafe { drop(Box::from_raw(tx_ptr)) };
}

#[test]
fn interface_null_self_reports_error() {
    let mut err = ok_err();
    let r = counters::weaveffi_counters_Counter_value(std::ptr::null(), &mut err);
    assert_eq!(r, 0);
    assert_ne!(err.code, 0);
    abi::error_clear(&mut err);
}

#[test]
fn producer_panic_reports_panic_code() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(0, &mut err);

    counters::weaveffi_counters_Counter_explode(c, &mut err);
    assert_eq!(err.code, abi::PANIC_ERROR_CODE);
    assert!(c_ptr_to_string(err.message)
        .unwrap()
        .contains("counter exploded"));
    abi::error_clear(&mut err);

    // The object is still usable and the error slot resets on the next call.
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 0);
    assert_eq!(err.code, 0);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn deprecated_export_thunk_compiles_and_runs() {
    let mut err = ok_err();
    assert_eq!(legacy::weaveffi_legacy_add_one(41, &mut err), 42);
    assert_eq!(err.code, 0);

    // Calling the deprecated thunk would warn at this site, but the generated
    // thunk's own `#[allow(deprecated)]` keeps the macro expansion clean.
    #[allow(deprecated)]
    let bumped = legacy::weaveffi_legacy_bump(41, &mut err);
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
    assert_eq!(quota::weaveffi_quota_consume(30, &mut err), 70);
    assert_eq!(err.code, 0);
    assert!(err.payload_ptr.is_null());

    // A payload-carrying variant serializes its fields in declaration order.
    let r = quota::weaveffi_quota_consume(250, &mut err);
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, 3001);
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "quota exceeded");
    assert!(!err.payload_ptr.is_null());
    let payload = unsafe { std::slice::from_raw_parts(err.payload_ptr, err.payload_len) };
    let mut reader = abi::BufferReader::new(payload);
    assert_eq!(reader.read_i64().unwrap(), 100, "limit field");
    assert_eq!(reader.read_i64().unwrap(), 250, "used field");
    reader.expect_end().unwrap();
    abi::error_clear(&mut err);
    assert!(err.payload_ptr.is_null(), "clear releases the payload");

    // A unit variant reports code and message with no payload.
    let r = quota::weaveffi_quota_consume(-1, &mut err);
    assert_eq!(r, 0);
    assert_eq!(err.code, 3002);
    assert!(err.payload_ptr.is_null());
    abi::error_clear(&mut err);
}
