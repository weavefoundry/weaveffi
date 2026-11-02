//! End-to-end runtime tests for the producer macros.
//!
//! Each Rust module below declares part of one library with the attributes
//! (a library is one namespace, so the modules only organize the Rust code),
//! and the tests call the exported C symbols the way a C consumer does,
//! through `extern "C"` declarations, checking that arguments lift, results
//! lower, and errors flow through `out_err` as the C ABI (revision 6)
//! promises. This test crate is named `runtime`, so that's the prefix of
//! every symbol.

#![allow(unsafe_code)]
// The `extern` blocks name the runtime's opaque types (objects, iterators,
// cancel tokens) behind pointers, as the C header names incomplete structs.
#![allow(improper_ctypes)]

use std::mem::MaybeUninit;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use weaveffi::__rt::{
    self as rt, BufferWriter, CallbackArg, FfiCancelToken, FfiError, FfiType, IterHandle, Opt, Run,
    VtableHeader,
};

weaveffi::export_runtime!();

/// The crate's tag, under which every exported type implements its traits.
type Tag = __weaveffi::Tag;

/// How long a test waits for an async completion. Generous, so a loaded
/// machine can't turn a slow completion into a failure; a passing run never
/// waits this long.
const WAIT: Duration = Duration::from_secs(30);

pub mod demo {
    /// The demo error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum DemoError {
        /// division by zero
        #[weaveffi(message = "cannot divide by zero")]
        DivisionByZero = 100,
    }

    /// A C-style enum that crosses the ABI as its `int32_t` discriminant.
    #[weaveffi::enumeration]
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
    #[derive(Clone, Debug, PartialEq)]
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
    pub fn char_count(text: &str) -> i32 {
        text.chars().count() as i32
    }

    /// The address of a borrowed string's first byte, proving the thunk lent
    /// the caller's bytes instead of copying them.
    #[weaveffi::export]
    pub fn text_addr(text: &str) -> u64 {
        text.as_ptr() as u64
    }

    /// The address of a borrowed byte slice's first byte.
    #[weaveffi::export]
    pub fn blob_addr(data: &[u8]) -> u64 {
        data.as_ptr() as u64
    }

    /// The address of a borrowed typed array's first element.
    #[weaveffi::export]
    pub fn words_addr(words: &[i64]) -> u64 {
        words.as_ptr() as u64
    }

    /// Return an optional string depending on the flag.
    #[weaveffi::export]
    pub fn maybe_name(present: bool) -> Option<String> {
        present.then(|| "present".to_string())
    }

    /// Sum a list of scalars (a typed array).
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

    /// Build a point by value (returned as a value buffer).
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

    /// Negate a flag (a C `bool` in and out).
    #[weaveffi::export]
    pub fn negate(flag: bool) -> bool {
        !flag
    }

    /// Negate an optional flag.
    #[weaveffi::export]
    pub fn negate_opt(flag: Option<bool>) -> Option<bool> {
        flag.map(|f| !f)
    }

    /// The next color, if any (an optional enum in and out).
    #[weaveffi::export]
    pub fn next_color(color: Option<Color>) -> Option<Color> {
        color.map(|c| match c {
            Color::Red => Color::Green,
            Color::Green => Color::Blue,
            Color::Blue => Color::Red,
        })
    }
}

/// A record declared in one Rust module and used by functions in another:
/// rustc resolves every type through its traits, wherever it's declared.
pub mod warehouse {
    /// A record owned by the `warehouse` module.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
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

pub mod dispatch {
    use super::demo::Color;
    use super::warehouse::Crate;

    /// Read another module's record's id (record parameter by value).
    #[weaveffi::export]
    pub fn crate_id(item: Crate) -> i64 {
        item.id
    }

    /// Return a relabeled copy (another module's record in and out).
    #[weaveffi::export]
    pub fn relabel(item: Crate, label: String) -> super::warehouse::Crate {
        Crate { id: item.id, label }
    }

    /// Another module's C-style enum crosses as its discriminant, too.
    #[weaveffi::export]
    pub fn is_red(color: Color) -> bool {
        color == Color::Red
    }
}

/// Callback interfaces at full width: returns of every family adopted from
/// the consumer, a method that throws a domain decoded into its typed error,
/// optional callback parameters, a type alias, and `#[cfg]`'d items.
pub mod rich {
    use std::sync::Arc;

    use weaveffi::ForeignError;

    /// The lookup error domain; `Missing` carries the key.
    #[weaveffi::error]
    #[derive(Debug, PartialEq)]
    #[repr(i32)]
    pub enum LookupError {
        /// no such key
        #[weaveffi(message = "missing {key}")]
        Missing {
            /// The key that wasn't found.
            key: String,
        } = 1,
        /// the source is busy
        #[weaveffi(message = "busy")]
        Busy = 2,
        /// the source failed some other way
        #[weaveffi(message = "foreign {code}: {message}")]
        Foreign {
            /// The failure's runtime code.
            code: i32,
            /// The consumer's message.
            message: String,
        } = 3,
    }

    impl From<ForeignError> for LookupError {
        fn from(e: ForeignError) -> Self {
            Self::Foreign {
                code: e.code,
                message: e.message,
            }
        }
    }

    /// A key, spelled through an alias rustc resolves.
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

    #[weaveffi::export]
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

    // Compiled out: there's no member thunk or contract function (the
    // generated code calls `hidden`, so a stray thunk wouldn't compile).
    #[cfg(not(test))]
    #[weaveffi::export]
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
        fn lookup(&self, key: &str) -> Result<i64, LookupError>;
    }

    /// Call every value-returning method and describe the results.
    #[weaveffi::export]
    pub fn describe_source(source: Arc<dyn Source>) -> Result<String, ForeignError> {
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
            Err(e) => e.to_string(),
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

    /// Present in no build: no thunk, no contract function.
    #[cfg(not(test))]
    #[weaveffi::export]
    pub fn never() -> i32 {
        0
    }
}

pub mod maps {
    use std::collections::BTreeMap;

    /// Double every value in a string-keyed map (map in, map out).
    #[weaveffi::export]
    pub fn double_scores(scores: BTreeMap<String, i32>) -> BTreeMap<String, i32> {
        scores.into_iter().map(|(k, v)| (k, v * 2)).collect()
    }

    /// Sum a map's values (map parameter, scalar return).
    #[weaveffi::export]
    pub fn total_scores(scores: BTreeMap<String, i32>) -> i32 {
        scores.values().sum()
    }
}

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
    pub fn describe_shape(shape: &Shape) -> String {
        match shape {
            Shape::Empty => "empty".to_string(),
            Shape::Circle { radius } => format!("circle({radius})"),
            Shape::Labeled { label, count } => format!("{label}x{count}"),
        }
    }
}

pub mod stream {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use weaveffi::__rt::{FfiError, IterHandle};

    /// Yield `count` greetings lazily as an `iter<string>`.
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
    pub static REENTRANT_HANDLE: AtomicUsize = AtomicUsize::new(0);

    /// Yield three elements, each the error code a nested `_next` on the
    /// same handle reported from inside the producer's `next`.
    #[weaveffi::export]
    pub fn reentrant() -> weaveffi::Iter<i32> {
        weaveffi::Iter::new((0..3).map(|_| {
            let handle = REENTRANT_HANDLE.load(Ordering::SeqCst);
            let mut err = FfiError::default();
            let mut item = 0;
            unsafe {
                super::runtime_ReentrantIterator_next(
                    handle as *mut IterHandle<i32>,
                    &mut item,
                    &mut err,
                )
            };
            err.code
        }))
    }
}

pub mod bus {
    use std::sync::{Arc, Mutex, PoisonError};

    use weaveffi::ForeignError;

    /// A message priority (a C-style enum crossing a callback boundary).
    #[weaveffi::enumeration]
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
        /// Inspect a shared object, given one reference to each.
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

    #[weaveffi::export]
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

    #[weaveffi::export]
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
        /// subscriber failure fails the call with the subscriber's message.
        /// The list is snapshotted so no lock is held while a callback runs.
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

    /// Hand the subscriber a ticker object, alone and then with an `alt`.
    #[weaveffi::export]
    pub fn tick(subscriber: Arc<dyn Subscriber>, value: i64) -> Result<bool, ForeignError> {
        let ticker = Arc::new(Ticker::new(value));
        Ok(subscriber.on_ticker(ticker.clone(), None)?
            && subscriber.on_ticker(ticker.clone(), Some(ticker))?)
    }

    /// Classify through the subscriber and describe the outcome, handling a
    /// consumer failure as an ordinary value.
    #[weaveffi::export]
    pub fn classify_or_explain(subscriber: Arc<dyn Subscriber>, weight: i32) -> String {
        match subscriber.classify(weight) {
            Ok(p) => format!("ok {p:?}"),
            Err(e) => format!("err {}: {}", e.code, e.message),
        }
    }

    /// Hand the subscriber a ticker and describe the outcome.
    #[weaveffi::export]
    pub fn tick_or_explain(subscriber: Arc<dyn Subscriber>, value: i64) -> String {
        match subscriber.on_ticker(Arc::new(Ticker::new(value)), None) {
            Ok(b) => format!("ok {b}"),
            Err(e) => format!("err {}: {}", e.code, e.message),
        }
    }

    /// Weigh through the subscriber and describe the outcome.
    #[weaveffi::export]
    pub fn weigh_or_explain(subscriber: Arc<dyn Subscriber>, weight: i32) -> String {
        match subscriber.weigh(weight) {
            Ok(v) => format!("ok {v}"),
            Err(e) => format!("err {}: {}", e.code, e.message),
        }
    }
}

pub mod tasks {
    /// A tag for the thread running the call, to observe the executor.
    #[weaveffi::export]
    pub async fn thread_tag() -> String {
        let t = std::thread::current();
        format!("{:?} {}", t.id(), t.name().unwrap_or(""))
    }

    /// The task error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// arithmetic overflow
        Overflow = 1,
    }

    /// The by-value result an async task completes with.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
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

    /// Shout a borrowed string asynchronously (a run in, a run out).
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
    pub async fn wait_forever(cancel: weaveffi::CancelToken) -> i32 {
        let _keep = cancel;
        std::future::pending::<()>().await;
        0
    }

    /// Complete with nothing (a completion without a result slot).
    #[weaveffi::export]
    pub async fn tidy(fail: bool) -> Result<(), TaskError> {
        if fail {
            Err(TaskError::Overflow)
        } else {
            Ok(())
        }
    }
}

/// An interface, a C-style enum, and a record declared in one Rust module
/// and used from another.
pub mod outer {
    use std::sync::Arc;

    /// An interface declared in the parent module.
    #[weaveffi::interface]
    pub struct Session {
        /// The session id.
        pub id: i64,
    }

    #[weaveffi::export]
    impl Session {
        /// Open a session.
        pub fn open(id: i64) -> Self {
            Self { id }
        }
    }

    /// A C-style enum declared in the parent module.
    #[weaveffi::enumeration]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Level {
        /// Low.
        Low = 1,
        /// High.
        High = 2,
    }

    /// Return the same session (an `Arc`-typed parameter and return).
    #[weaveffi::export]
    pub fn share(session: Arc<Session>) -> Arc<Session> {
        session
    }

    pub mod inner {
        use std::sync::Arc;

        /// A by-value record produced by the nested module.
        #[weaveffi::record]
        #[derive(Clone)]
        pub struct Report {
            /// Ten times the session id.
            pub score: i64,
            /// The session the report is about (an object token in the
            /// buffer).
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

/// A declared domain whose message interpolates a field, and functions
/// whose error type isn't a domain (`throws any`).
pub mod vault {
    /// The vault's declared error domain: the codes consumers can match on.
    #[weaveffi::error]
    #[derive(Debug)]
    #[repr(i32)]
    pub enum VaultError {
        /// entry not found
        NotFound = 2001,
        /// vault sealed
        #[weaveffi(message = "vault sealed: {reason}")]
        Sealed {
            /// Why the vault is sealed.
            reason: String,
        } = 2002,
    }

    /// The producer's internal failure type: not a domain, so a function
    /// returning it throws `any` and reports its `Display` with code -1.
    #[derive(Debug)]
    pub struct VaultFailure(String);

    impl std::fmt::Display for VaultFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "vault failure: {}", self.0)
        }
    }

    impl std::error::Error for VaultFailure {}

    /// Fetch a doubled value, failing with a domain code for invalid keys.
    #[weaveffi::export]
    pub fn fetch(key: i64) -> Result<i64, VaultError> {
        match key {
            0 => Err(VaultError::NotFound),
            n if n < 0 => Err(VaultError::Sealed {
                reason: "negative key".to_string(),
            }),
            n => Ok(n * 2),
        }
    }

    /// Fetch, failing with an untyped error converted with `?`.
    #[weaveffi::export]
    pub fn fetch_any(key: i64) -> Result<i64, weaveffi::Error> {
        if key == 0 {
            return Err(VaultFailure("no key".to_string()).into());
        }
        Ok(key)
    }

    /// Parse an integer: any std error converts into `weaveffi::Error`.
    #[weaveffi::export]
    pub fn parse_int(text: &str) -> Result<i64, weaveffi::Error> {
        Ok(text.parse()?)
    }

    /// Fail with a `String` message (`throws any`).
    #[weaveffi::export]
    pub fn refuse(why: String) -> Result<(), String> {
        Err(why)
    }
}

/// A `#[deprecated]` export. The generated thunk must still *call* the
/// deprecated function, so it carries an `#[allow(deprecated)]` of its own;
/// this module compiling under the workspace's `-D warnings` policy is the
/// proof.
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

/// An interface with constructors, methods, statics, and a destructor.
pub mod counters {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    /// The counters error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CounterError {
        /// start value out of range
        #[weaveffi(message = "step must be positive")]
        OutOfRange = 1,
    }

    /// A monotonic counter, exported as an interface.
    #[weaveffi::interface]
    pub struct Counter {
        value: AtomicI64,
        step: i64,
    }

    #[weaveffi::export]
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

        /// Return a new reference to this same counter (`Arc<Self>`
        /// receiver and return).
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

        /// Yield `n` fresh counters lazily (interface elements in an
        /// iterator).
        pub fn fan_out(&self, n: i32) -> weaveffi::Iter<Arc<Counter>> {
            let base = self.value();
            weaveffi::Iter::new((0..i64::from(n)).map(move |i| Arc::new(Counter::new(base + i))))
        }

        /// Read the value asynchronously (an async method retains `self`).
        pub async fn value_later(&self) -> i64 {
            self.value()
        }

        /// Return the same counter asynchronously (an async object result).
        pub async fn snapshot_later(self: Arc<Self>) -> Arc<Counter> {
            self
        }

        /// Panic on purpose, proving panics surface as errors, not aborts.
        pub fn explode(&self) {
            panic!("counter exploded");
        }

        /// The default start value (a static under the interface).
        pub fn default_start() -> i64 {
            0
        }

        /// Create a counter asynchronously: an async factory is a static
        /// (constructors are synchronous) that returns the interface.
        pub async fn start_later(start: i64) -> Self {
            Self::new(start)
        }

        /// Skipped: not exported, though public.
        #[weaveffi(skip)]
        pub fn internal(&self) -> Box<dyn Fn() -> i64> {
            Box::new(|| -1)
        }

        // A private helper: not exported.
        #[allow(dead_code)]
        fn private(&self) -> i64 {
            -1
        }
    }

    /// A free function taking the interface by reference.
    #[weaveffi::export]
    pub fn read_twice(counter: &Counter) -> i64 {
        counter.value() * 2
    }
}

/// An error domain whose code carries structured payload fields: the
/// variant's named fields travel through the error's `payload_ptr` and
/// `payload_len`, encoded as a value buffer.
pub mod quota {
    /// The quota error domain. `Exceeded` carries a structured payload.
    #[weaveffi::error]
    #[derive(Debug)]
    #[repr(i32)]
    pub enum QuotaError {
        /// quota exceeded
        #[weaveffi(message = "used {used} of {limit}")]
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

// ── The C declarations, as a consumer writes them ──────────────────────

/// A completion that receives a result.
type Done<R> = unsafe extern "C" fn(*mut c_void, *mut FfiError, R);
/// A completion of a function that returns nothing.
type DoneUnit = unsafe extern "C" fn(*mut c_void, *mut FfiError);

unsafe extern "C" {
    // The runtime surface.
    fn runtime_error_set(err: *mut FfiError, code: i32, ptr: *const u8, len: usize);
    fn runtime_error_set_payload(err: *mut FfiError, ptr: *const u8, len: usize);
    fn runtime_error_clear(err: *mut FfiError);
    fn runtime_error_free(err: *mut FfiError);
    fn runtime_alloc(len: usize) -> *mut u8;
    fn runtime_run_free(run: Run);
    fn runtime_cancel_token_create() -> *mut FfiCancelToken;
    fn runtime_cancel_token_cancel(token: *mut FfiCancelToken);
    fn runtime_cancel_token_is_cancelled(token: *const FfiCancelToken) -> bool;
    fn runtime_cancel_token_destroy(token: *mut FfiCancelToken);

    // demo
    fn runtime_add(a: i32, b: i32, err: *mut FfiError) -> i32;
    fn runtime_checked_div(a: i32, b: i32, err: *mut FfiError) -> i32;
    fn runtime_greet(name: Run, err: *mut FfiError) -> Run;
    fn runtime_char_count(text: Run, err: *mut FfiError) -> i32;
    fn runtime_text_addr(text: Run, err: *mut FfiError) -> u64;
    fn runtime_blob_addr(data: Run, err: *mut FfiError) -> u64;
    fn runtime_words_addr(words: Run, err: *mut FfiError) -> u64;
    fn runtime_maybe_name(present: u8, err: *mut FfiError) -> Run;
    fn runtime_sum(xs: Run, err: *mut FfiError) -> i32;
    fn runtime_join(parts: Run, err: *mut FfiError) -> Run;
    fn runtime_byte_count(data: Run, err: *mut FfiError) -> i32;
    fn runtime_make_point(x: i32, err: *mut FfiError) -> Run;
    fn runtime_point_x(p: Run, err: *mut FfiError) -> i32;
    fn runtime_echo_color(c: i32, err: *mut FfiError) -> i32;
    fn runtime_negate(flag: u8, err: *mut FfiError) -> u8;
    fn runtime_negate_opt(flag: Opt<u8>, err: *mut FfiError) -> Opt<u8>;
    fn runtime_next_color(color: Opt<i32>, err: *mut FfiError) -> Opt<i32>;

    // warehouse and dispatch
    fn runtime_make_crate(id: i64, label: Run, err: *mut FfiError) -> Run;
    fn runtime_crate_id(item: Run, err: *mut FfiError) -> i64;
    fn runtime_relabel(item: Run, label: Run, err: *mut FfiError) -> Run;
    fn runtime_is_red(color: i32, err: *mut FfiError) -> u8;

    // rich
    fn runtime_Token_new(n: i64, err: *mut FfiError) -> *const rich::Token;
    fn runtime_describe_source(source: CallbackArg<SourceVtable>, err: *mut FfiError) -> Run;
    fn runtime_lookup_via(source: CallbackArg<SourceVtable>, key: Run, err: *mut FfiError) -> Run;
    fn runtime_has_source(source: CallbackArg<SourceVtable>, label: Run, err: *mut FfiError)
        -> Run;
    fn runtime_always(err: *mut FfiError) -> i32;

    // maps
    fn runtime_double_scores(scores: Run, err: *mut FfiError) -> Run;
    fn runtime_total_scores(scores: Run, err: *mut FfiError) -> i32;

    // geom
    fn runtime_describe_shape(shape: Run, err: *mut FfiError) -> Run;

    // stream
    fn runtime_greetings(count: i32, err: *mut FfiError) -> *mut IterHandle<String>;
    fn runtime_GreetingsIterator_next(
        iter: *mut IterHandle<String>,
        out_item: *mut Run,
        err: *mut FfiError,
    ) -> i32;
    fn runtime_GreetingsIterator_destroy(iter: *mut IterHandle<String>);
    fn runtime_squares(count: i32, err: *mut FfiError) -> *mut IterHandle<i32>;
    fn runtime_SquaresIterator_next(
        iter: *mut IterHandle<i32>,
        out_item: *mut i32,
        err: *mut FfiError,
    ) -> i32;
    fn runtime_SquaresIterator_destroy(iter: *mut IterHandle<i32>);
    fn runtime_reentrant(err: *mut FfiError) -> *mut IterHandle<i32>;
    fn runtime_ReentrantIterator_next(
        iter: *mut IterHandle<i32>,
        out_item: *mut i32,
        err: *mut FfiError,
    ) -> i32;
    fn runtime_ReentrantIterator_destroy(iter: *mut IterHandle<i32>);

    // bus
    fn runtime_Ticker_value(this: *const bus::Ticker, err: *mut FfiError) -> i64;
    fn runtime_Ticker_destroy(this: *mut bus::Ticker);
    fn runtime_Bus_new(err: *mut FfiError) -> *const bus::Bus;
    fn runtime_Bus_subscribe(
        this: *const bus::Bus,
        subscriber: CallbackArg<SubscriberVtable>,
        err: *mut FfiError,
    );
    fn runtime_Bus_publish(
        this: *const bus::Bus,
        text: Run,
        weight: i32,
        err: *mut FfiError,
    ) -> i64;
    fn runtime_Bus_publish_later(
        this: *const bus::Bus,
        text: Run,
        weight: i32,
        callback: Option<Done<i64>>,
        context: *mut c_void,
    );
    fn runtime_Bus_clear(this: *const bus::Bus, err: *mut FfiError);
    fn runtime_Bus_destroy(this: *mut bus::Bus);
    fn runtime_classify_once(
        subscriber: CallbackArg<SubscriberVtable>,
        weight: i32,
        err: *mut FfiError,
    ) -> i32;
    fn runtime_tick(
        subscriber: CallbackArg<SubscriberVtable>,
        value: i64,
        err: *mut FfiError,
    ) -> u8;
    fn runtime_classify_or_explain(
        subscriber: CallbackArg<SubscriberVtable>,
        weight: i32,
        err: *mut FfiError,
    ) -> Run;
    fn runtime_tick_or_explain(
        subscriber: CallbackArg<SubscriberVtable>,
        value: i64,
        err: *mut FfiError,
    ) -> Run;
    fn runtime_weigh_or_explain(
        subscriber: CallbackArg<SubscriberVtable>,
        weight: i32,
        err: *mut FfiError,
    ) -> Run;

    // tasks
    fn runtime_thread_tag(callback: Option<Done<Run>>, context: *mut c_void);
    fn runtime_run_task(name: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn runtime_shout(text: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn runtime_checked_add(a: i32, b: i32, callback: Option<Done<i32>>, context: *mut c_void);
    fn runtime_wait_forever(
        cancel_token: *const FfiCancelToken,
        callback: Option<Done<i32>>,
        context: *mut c_void,
    );
    fn runtime_tidy(fail: u8, callback: Option<DoneUnit>, context: *mut c_void);

    // outer
    fn runtime_Session_open(id: i64, err: *mut FfiError) -> *const outer::Session;
    fn runtime_Session_clone(this: *const outer::Session) -> *const outer::Session;
    fn runtime_Session_destroy(this: *mut outer::Session);
    fn runtime_share(session: *const outer::Session, err: *mut FfiError) -> *const outer::Session;
    fn runtime_summarize(
        session: *const outer::Session,
        keep: *const outer::Session,
        err: *mut FfiError,
    ) -> Run;
    fn runtime_attach(session: *const outer::Session, err: *mut FfiError) -> Run;
    fn runtime_session_of(report: Run, err: *mut FfiError) -> *const outer::Session;
    fn runtime_raise(level: i32, err: *mut FfiError) -> i32;

    // vault
    fn runtime_fetch(key: i64, err: *mut FfiError) -> i64;
    fn runtime_fetch_any(key: i64, err: *mut FfiError) -> i64;
    fn runtime_parse_int(text: Run, err: *mut FfiError) -> i64;
    fn runtime_refuse(why: Run, err: *mut FfiError);

    // legacy
    fn runtime_add_one(value: i64, err: *mut FfiError) -> i64;
    fn runtime_bump(value: i64, err: *mut FfiError) -> i64;

    // counters
    fn runtime_Counter_new(start: i64, err: *mut FfiError) -> *const counters::Counter;
    fn runtime_Counter_with_step(
        start: i64,
        step: i64,
        err: *mut FfiError,
    ) -> *const counters::Counter;
    fn runtime_Counter_increment(this: *const counters::Counter, err: *mut FfiError) -> i64;
    fn runtime_Counter_value(this: *const counters::Counter, err: *mut FfiError) -> i64;
    fn runtime_Counter_label(
        this: *const counters::Counter,
        prefix: Run,
        err: *mut FfiError,
    ) -> Run;
    fn runtime_Counter_snapshot(
        this: *const counters::Counter,
        err: *mut FfiError,
    ) -> *const counters::Counter;
    fn runtime_Counter_share(
        this: *const counters::Counter,
        err: *mut FfiError,
    ) -> *const counters::Counter;
    fn runtime_Counter_larger(
        this: *const counters::Counter,
        other: *const counters::Counter,
        floor: i64,
        err: *mut FfiError,
    ) -> *const counters::Counter;
    fn runtime_Counter_fan_out(
        this: *const counters::Counter,
        n: i32,
        err: *mut FfiError,
    ) -> *mut IterHandle<Arc<counters::Counter>>;
    fn runtime_Counter_FanOutIterator_next(
        iter: *mut IterHandle<Arc<counters::Counter>>,
        out_item: *mut *const counters::Counter,
        err: *mut FfiError,
    ) -> i32;
    fn runtime_Counter_FanOutIterator_destroy(iter: *mut IterHandle<Arc<counters::Counter>>);
    fn runtime_Counter_value_later(
        this: *const counters::Counter,
        callback: Option<Done<i64>>,
        context: *mut c_void,
    );
    fn runtime_Counter_snapshot_later(
        this: *const counters::Counter,
        callback: Option<Done<*const counters::Counter>>,
        context: *mut c_void,
    );
    fn runtime_Counter_explode(this: *const counters::Counter, err: *mut FfiError);
    fn runtime_Counter_default_start(err: *mut FfiError) -> i64;
    fn runtime_Counter_start_later(
        start: i64,
        callback: Option<Done<*const counters::Counter>>,
        context: *mut c_void,
    );
    fn runtime_Counter_clone(this: *const counters::Counter) -> *const counters::Counter;
    fn runtime_Counter_destroy(this: *mut counters::Counter);
    fn runtime_read_twice(counter: *const counters::Counter, err: *mut FfiError) -> i64;

    // quota
    fn runtime_consume(amount: i64, err: *mut FfiError) -> i64;
}

// ── Consumer helpers ────────────────────────────────────────────────────

/// An error slot full of garbage: a call overwrites `out_err` without
/// reading it, so a consumer may pass uninitialized memory.
fn garbage_err() -> MaybeUninit<FfiError> {
    let mut slot = MaybeUninit::<FfiError>::uninit();
    unsafe { std::ptr::write_bytes(slot.as_mut_ptr(), 0xA5, 1) };
    slot
}

/// The message in `err`, or `""`.
fn message(err: &FfiError) -> String {
    unsafe { err.message_str() }.unwrap_or_default().to_string()
}

/// Read a failure's code and message, then release it, as a binding does.
fn take_failure(err: &mut FfiError) -> (i32, String) {
    let out = (err.code, message(err));
    unsafe { runtime_error_clear(err) };
    out
}

/// A borrowed string argument.
fn text(s: &str) -> Run {
    Run::borrowing(s.as_bytes())
}

/// A borrowed typed-array argument.
fn array<T: Copy>(values: &[T]) -> Run {
    Run::borrowing(unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values))
    })
}

/// The value-buffer encoding of `value`.
fn encode<T: FfiType<Tag>>(value: &T) -> Vec<u8> {
    rt::encode(|w| value.write(w))
}

/// Copy a returned string and release its run.
fn take_string(run: Run) -> String {
    assert_eq!(run.ptr as usize % 8, 0, "every run is 8-aligned");
    let s = unsafe { run.text() }.expect("valid UTF-8").to_string();
    unsafe { runtime_run_free(run) };
    s
}

/// Decode a returned value buffer and release its run.
fn take<T: FfiType<Tag>>(run: Run) -> T {
    assert!(!run.ptr.is_null(), "a returned buffer is never empty");
    // SAFETY: a producer's return; its object tokens are ours to adopt, once.
    let value = unsafe { rt::decode_run(run, |r| T::read(r)) }.expect("a well-formed buffer");
    unsafe { runtime_run_free(run) };
    value
}

/// A run the consumer allocates with `{prefix}_alloc` and hands to the
/// producer, which adopts it (a callback method's return).
fn give(bytes: &[u8]) -> Run {
    if bytes.is_empty() {
        return Run::EMPTY;
    }
    let ptr = unsafe { runtime_alloc(bytes.len()) };
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    Run {
        ptr,
        len: bytes.len(),
    }
}

/// Report a consumer failure through the exported `{prefix}_error_set`.
fn fail(err: *mut FfiError, code: i32, msg: &str) {
    unsafe { runtime_error_set(err, code, msg.as_ptr(), msg.len()) };
}

// ── Plain calls ─────────────────────────────────────────────────────────

#[test]
fn scalar_call_sets_ok() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_add(2, 40, &mut err) }, 42);
    assert_eq!(err.code, 0);
    assert!(err.message_ptr.is_null());
}

#[test]
fn fallible_ok_and_err_paths_use_display_for_the_message() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_checked_div(10, 2, &mut err) }, 5);
    assert_eq!(err.code, 0);

    let r = unsafe { runtime_checked_div(1, 0, &mut err) };
    assert_eq!(r, 0, "a failure returns the zero value");
    assert_eq!(
        take_failure(&mut err),
        (100, "cannot divide by zero".to_string()),
        "the domain code and its Display output"
    );
    assert!(err.message_ptr.is_null(), "clear releases the message");
}

#[test]
fn out_err_is_overwritten_never_read() {
    // Success stores a cleared error over the garbage.
    let mut slot = garbage_err();
    assert_eq!(unsafe { runtime_add(1, 2, slot.as_mut_ptr()) }, 3);
    let err = unsafe { slot.assume_init() };
    assert_eq!(err.code, 0);
    assert!(err.message_ptr.is_null() && err.payload_ptr.is_null());
    assert_eq!((err.message_len, err.payload_len), (0, 0));

    // A failure stores its code and message over the garbage, too.
    let mut slot = garbage_err();
    assert_eq!(unsafe { runtime_checked_div(1, 0, slot.as_mut_ptr()) }, 0);
    let mut err = unsafe { slot.assume_init() };
    assert_eq!(take_failure(&mut err).0, 100);

    // A failure with a payload, and a marshalling failure.
    let mut slot = garbage_err();
    unsafe { runtime_consume(500, slot.as_mut_ptr()) };
    let mut err = unsafe { slot.assume_init() };
    assert!(!err.payload_ptr.is_null());
    assert_eq!(take_failure(&mut err).0, 3001);
    let mut slot = garbage_err();
    unsafe { runtime_char_count(Run::borrowing(&[0xff]), slot.as_mut_ptr()) };
    let mut err = unsafe { slot.assume_init() };
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    // So a struct reused across calls needs a clear only after a failure.
    let mut err = FfiError::default();
    for _ in 0..3 {
        unsafe { runtime_checked_div(1, 0, &mut err) };
        assert_eq!(take_failure(&mut err).0, 100);
        assert_eq!(unsafe { runtime_add(1, 1, &mut err) }, 2);
        assert_eq!(err.code, 0);
    }
}

#[test]
fn owned_string_round_trip() {
    let mut err = FfiError::default();
    let out = unsafe { runtime_greet(text("alice"), &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(out), "hi alice");
}

#[test]
fn interior_nul_in_a_returned_string_round_trips() {
    let mut err = FfiError::default();
    let out = unsafe { runtime_greet(text("a\0b"), &mut err) };
    assert_eq!(out.len, 6);
    assert_eq!(take_string(out), "hi a\0b");
}

#[test]
fn borrowed_params_are_not_copied() {
    let mut err = FfiError::default();
    // A slice of a larger buffer: not NUL-terminated, and borrowed in place.
    let backing = "héllo world";
    let slice = &backing[..6];
    assert_eq!(unsafe { runtime_char_count(text(slice), &mut err) }, 5);
    let addr = unsafe { runtime_text_addr(text(slice), &mut err) };
    assert_eq!(
        addr,
        slice.as_ptr() as u64,
        "the thunk lent the caller's bytes"
    );

    let data = [1u8, 2, 3];
    let addr = unsafe { runtime_blob_addr(Run::borrowing(&data), &mut err) };
    assert_eq!(addr, data.as_ptr() as u64);

    let words = [1i64, 2, 3];
    let addr = unsafe { runtime_words_addr(array(&words), &mut err) };
    assert_eq!(addr, words.as_ptr() as u64, "a typed array is lent, too");

    // An empty string may be passed as the empty run.
    assert_eq!(unsafe { runtime_char_count(Run::EMPTY, &mut err) }, 0);
    assert_eq!(err.code, 0);
}

#[test]
fn malformed_runs_are_marshalling_errors() {
    let mut err = FfiError::default();
    let bad = [0xFFu8, 0xFE];
    assert_eq!(
        unsafe { runtime_char_count(Run::borrowing(&bad), &mut err) },
        0
    );
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    let null_with_len = Run {
        ptr: std::ptr::null(),
        len: 3,
    };
    assert_eq!(unsafe { runtime_char_count(null_with_len, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert!(msg.contains("null run"), "{msg}");

    // A typed array whose length isn't a multiple of its element size, or
    // that isn't aligned for it.
    let words = [1i64, 2];
    let ragged = Run {
        len: 12,
        ..array(&words)
    };
    assert_eq!(unsafe { runtime_words_addr(ragged, &mut err) }, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
    let shifted = Run {
        ptr: unsafe { array(&words).ptr.add(4) },
        len: 8,
    };
    assert_eq!(unsafe { runtime_words_addr(shifted, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert!(msg.contains("aligned"), "{msg}");
}

#[test]
fn optional_string_return_is_a_value_buffer() {
    let mut err = FfiError::default();
    let some = unsafe { runtime_maybe_name(1, &mut err) };
    assert_eq!(take::<Option<String>>(some), Some("present".to_string()));
    let none = unsafe { runtime_maybe_name(0, &mut err) };
    assert_eq!(take::<Option<String>>(none), None);
}

#[test]
fn scalar_list_param_is_a_typed_array() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_sum(array(&[3i32, 4, 5]), &mut err) }, 12);
    assert_eq!(unsafe { runtime_sum(Run::EMPTY, &mut err) }, 0);
    assert_eq!(err.code, 0);
    let null_with_len = Run {
        ptr: std::ptr::null(),
        len: 8,
    };
    assert_eq!(unsafe { runtime_sum(null_with_len, &mut err) }, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn string_list_param_is_a_value_buffer() {
    let mut err = FfiError::default();
    let parts = encode(&vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    let out = unsafe { runtime_join(Run::borrowing(&parts), &mut err) };
    assert_eq!(take_string(out), "a,b,c");
}

#[test]
fn malformed_buffer_param_reports_an_error() {
    let mut err = FfiError::default();
    // A truncated encoding (a count with no elements) is rejected through
    // `out_err`, never decoded partially.
    let bad = [9u8, 0, 0, 0];
    let joined = unsafe { runtime_join(Run::borrowing(&bad), &mut err) };
    assert_eq!(joined, Run::EMPTY, "a failure returns the empty run");
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    // Trailing bytes are malformed, too.
    let mut parts = encode(&vec!["a".to_string()]);
    parts.push(0);
    unsafe { runtime_join(Run::borrowing(&parts), &mut err) };
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn bytes_param() {
    let mut err = FfiError::default();
    let data = [1u8, 2, 3, 4, 5];
    assert_eq!(
        unsafe { runtime_byte_count(Run::borrowing(&data), &mut err) },
        5
    );
}

#[test]
fn c_style_enum_in_and_out() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_echo_color(2, &mut err) }, 2);
    assert_eq!(err.code, 0);
    assert_eq!(unsafe { runtime_echo_color(9, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert_eq!(
        msg, "c: 9 is not a valid Color",
        "the parameter and its value"
    );
}

#[test]
fn bools_are_checked_bytes() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_negate(1, &mut err) }, 0);
    assert_eq!(unsafe { runtime_negate(0, &mut err) }, 1);
    assert_eq!(err.code, 0);
    // A byte other than 0 or 1 is a marshalling failure, not undefined
    // behavior.
    assert_eq!(unsafe { runtime_negate(2, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert_eq!(msg, "flag: 2 is not a valid bool");
    assert_eq!(unsafe { runtime_maybe_name(0xff, &mut err) }, Run::EMPTY);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    // An optional's `has` and an optional bool's value are checked too.
    let r = unsafe { runtime_negate_opt(Opt::some(1), &mut err) };
    assert_eq!((r.has, r.value), (1, 0));
    let r = unsafe { runtime_negate_opt(Opt { has: 0, value: 0 }, &mut err) };
    assert_eq!((r.has, r.value), (0, 0), "an absent value is 0");
    for bad in [Opt { has: 2, value: 0 }, Opt { has: 1, value: 7 }] {
        let r = unsafe { runtime_negate_opt(bad, &mut err) };
        assert_eq!((r.has, r.value), (0, 0));
        assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
    }
    let r = unsafe { runtime_next_color(Opt { has: 3, value: 0 }, &mut err) };
    assert_eq!(r.has, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn optional_enums_are_opt_i32() {
    let mut err = FfiError::default();
    let r = unsafe { runtime_next_color(Opt::some(2), &mut err) };
    assert_eq!((r.has, r.value), (1, 0));
    let r = unsafe { runtime_next_color(Opt { has: 0, value: 0 }, &mut err) };
    assert_eq!((r.has, r.value), (0, 0));
    // A present value is checked; an absent one is ignored.
    let r = unsafe { runtime_next_color(Opt::some(7), &mut err) };
    assert_eq!(r.has, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
    let r = unsafe { runtime_next_color(Opt { has: 0, value: 7 }, &mut err) };
    assert_eq!((r.has, err.code), (0, 0));
}

#[test]
fn record_buffer_round_trip() {
    // A record is a value type: encoding and decoding must round-trip every
    // field, including the optional and enum fields.
    let original = demo::Point {
        x: 7,
        label: "corner".to_string(),
        nickname: Some("nw".to_string()),
        color: demo::Color::Blue,
    };
    let bytes = encode(&original);
    let back: demo::Point =
        unsafe { rt::decode_run(Run::borrowing(&bytes), |r| FfiType::<Tag>::read(r)) }.unwrap();
    assert_eq!(back, original);
}

#[test]
fn record_param_and_return_are_value_buffers() {
    let mut err = FfiError::default();
    let p = demo::Point {
        x: 41,
        label: "in".to_string(),
        nickname: None,
        color: demo::Color::Red,
    };
    let bytes = encode(&p);
    assert_eq!(
        unsafe { runtime_point_x(Run::borrowing(&bytes), &mut err) },
        41
    );
    assert_eq!(err.code, 0);

    let p: demo::Point = take(unsafe { runtime_make_point(99, &mut err) });
    assert_eq!(err.code, 0);
    assert_eq!(
        p,
        demo::Point {
            x: 99,
            label: "origin".to_string(),
            nickname: None,
            color: demo::Color::Green,
        }
    );
}

#[test]
fn types_from_other_rust_modules_cross_by_their_own_family() {
    let mut err = FfiError::default();
    let c: warehouse::Crate = take(unsafe { runtime_make_crate(7, text("widget"), &mut err) });
    assert_eq!(
        c,
        warehouse::Crate {
            id: 7,
            label: "widget".to_string()
        }
    );

    // `dispatch` takes `warehouse::Crate` as a value buffer.
    let bytes = encode(&c);
    assert_eq!(
        unsafe { runtime_crate_id(Run::borrowing(&bytes), &mut err) },
        7
    );
    let c2: warehouse::Crate =
        take(unsafe { runtime_relabel(Run::borrowing(&bytes), text("gadget"), &mut err) });
    assert_eq!((c2.id, c2.label.as_str()), (7, "gadget"));

    // ...and `demo::Color` as its discriminant: a C-style enum crosses
    // directly wherever it's declared.
    assert_eq!(unsafe { runtime_is_red(0, &mut err) }, 1);
    assert_eq!(unsafe { runtime_is_red(2, &mut err) }, 0);
    assert_eq!(unsafe { runtime_is_red(5, &mut err) }, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn map_param_and_return_are_value_buffers() {
    use std::collections::BTreeMap;
    let mut err = FfiError::default();
    let scores = BTreeMap::from([("a".to_string(), 2i32), ("b".to_string(), 1)]);
    let bytes = encode(&scores);

    let doubled: BTreeMap<String, i32> =
        take(unsafe { runtime_double_scores(Run::borrowing(&bytes), &mut err) });
    assert_eq!(err.code, 0);
    assert_eq!(
        doubled,
        BTreeMap::from([("a".to_string(), 4), ("b".to_string(), 2)])
    );
    assert_eq!(
        unsafe { runtime_total_scores(Run::borrowing(&bytes), &mut err) },
        3
    );
}

#[test]
fn duplicate_map_keys_are_marshalling_errors() {
    let mut err = FfiError::default();
    let mut w = BufferWriter::new();
    w.write_len(2);
    for v in [1i32, 2] {
        w.write_string("same");
        w.write_i32(v);
    }
    let bytes = w.finish();
    assert_eq!(
        unsafe { runtime_total_scores(Run::borrowing(&bytes), &mut err) },
        0
    );
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert!(msg.contains("duplicate map key"), "{msg}");
}

#[test]
fn record_optional_field_round_trips() {
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
        let bytes = encode(&w);
        let back: build::Widget =
            unsafe { rt::decode_run(Run::borrowing(&bytes), |r| FfiType::<Tag>::read(r)) }.unwrap();
        assert_eq!(back, w);
    }
}

#[test]
fn rich_enum_encodes_tag_then_fields() {
    // The encoding leads with the i32 tag (declaration order: Empty = 0,
    // Circle = 1, Labeled = 2), then the active variant's fields.
    assert_eq!(encode(&geom::Shape::Empty), [0, 0, 0, 0]);

    let circle = encode(&geom::Shape::Circle { radius: 2.5 });
    assert_eq!(&circle[..4], [1, 0, 0, 0]);
    assert_eq!(circle.len(), 4 + 8, "tag + f64 radius");

    let labeled = geom::Shape::Labeled {
        label: "hex".to_string(),
        count: 6,
    };
    let bytes = encode(&labeled);
    assert_eq!(&bytes[..4], [2, 0, 0, 0]);
    let back: geom::Shape =
        unsafe { rt::decode_run(Run::borrowing(&bytes), |r| FfiType::<Tag>::read(r)) }.unwrap();
    assert_eq!(back, labeled);

    // An out-of-range tag is a decode error, not a silent default.
    let bad = [9u8, 0, 0, 0];
    let r = unsafe {
        rt::decode_run(Run::borrowing(&bad), |r| {
            <geom::Shape as FfiType<Tag>>::read(r)
        })
    };
    assert!(r.is_err());
}

#[test]
fn rich_enum_param_is_a_value_buffer() {
    let mut err = FfiError::default();
    let circle = encode(&geom::Shape::Circle { radius: 2.5 });
    let d = unsafe { runtime_describe_shape(Run::borrowing(&circle), &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(d), "circle(2.5)");
    let bad = [9u8, 0, 0, 0];
    unsafe { runtime_describe_shape(Run::borrowing(&bad), &mut err) };
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

// ── Iterators ───────────────────────────────────────────────────────────

#[test]
fn iterator_string_elements() {
    let mut err = FfiError::default();
    let iter = unsafe { runtime_greetings(3, &mut err) };
    assert_eq!(err.code, 0);
    assert!(!iter.is_null());

    let mut got = Vec::new();
    loop {
        let mut item = Run::EMPTY;
        let has = unsafe { runtime_GreetingsIterator_next(iter, &mut item, &mut err) };
        assert_eq!(err.code, 0);
        if has == 0 {
            break;
        }
        assert_eq!(has, 1);
        got.push(take_string(item));
    }
    // Exhausted stays exhausted.
    let mut item = Run::EMPTY;
    assert_eq!(
        unsafe { runtime_GreetingsIterator_next(iter, &mut item, &mut err) },
        0
    );
    unsafe { runtime_GreetingsIterator_destroy(iter) };
    assert_eq!(got, ["hi 0", "hi 1", "hi 2"]);
}

#[test]
fn iterator_scalar_elements() {
    let mut err = FfiError::default();
    let iter = unsafe { runtime_squares(4, &mut err) };
    let mut got = Vec::new();
    let mut item = 0i32;
    while unsafe { runtime_SquaresIterator_next(iter, &mut item, &mut err) } == 1 {
        got.push(item);
    }
    assert_eq!(err.code, 0);
    unsafe { runtime_SquaresIterator_destroy(iter) };
    assert_eq!(got, [0, 1, 4, 9]);
}

#[test]
fn iterator_next_checks_its_arguments_before_pulling() {
    let mut err = FfiError::default();
    let iter = unsafe { runtime_squares(2, &mut err) };
    // A null `out_item` fails before an element is pulled, so none is lost.
    let r = unsafe { runtime_SquaresIterator_next(iter, std::ptr::null_mut(), &mut err) };
    assert_eq!((r, take_failure(&mut err).0), (0, rt::MARSHAL_ERROR_CODE));
    let mut item = -1;
    assert_eq!(
        unsafe { runtime_SquaresIterator_next(iter, &mut item, &mut err) },
        1
    );
    assert_eq!(item, 0, "the first element is still there");
    unsafe { runtime_SquaresIterator_destroy(iter) };

    // A null iterator is a marshalling failure, and destroying null a no-op.
    let r = unsafe { runtime_SquaresIterator_next(std::ptr::null_mut(), &mut item, &mut err) };
    assert_eq!((r, take_failure(&mut err).0), (0, rt::MARSHAL_ERROR_CODE));
    unsafe { runtime_SquaresIterator_destroy(std::ptr::null_mut()) };
}

#[test]
fn concurrent_iterator_next_yields_each_element_once_or_reports_busy() {
    const N: i32 = 2000;
    let mut err = FfiError::default();
    let iter = unsafe { runtime_squares(N, &mut err) };
    let addr = iter as usize;
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                let iter = addr as *mut IterHandle<i32>;
                let mut err = FfiError::default();
                let mut got = Vec::new();
                loop {
                    let mut item = 0i32;
                    let has = unsafe { runtime_SquaresIterator_next(iter, &mut item, &mut err) };
                    // A `_next` racing another fails instead of blocking.
                    if err.code == rt::MARSHAL_ERROR_CODE {
                        unsafe { runtime_error_clear(&mut err) };
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
    unsafe { runtime_SquaresIterator_destroy(iter) };
}

#[test]
fn a_reentrant_next_fails_instead_of_deadlocking() {
    let mut err = FfiError::default();
    let iter = unsafe { runtime_reentrant(&mut err) };
    stream::REENTRANT_HANDLE.store(iter as usize, Ordering::SeqCst);
    let mut got = Vec::new();
    let mut item = 0i32;
    while unsafe { runtime_ReentrantIterator_next(iter, &mut item, &mut err) } == 1 {
        assert_eq!(err.code, 0);
        got.push(item);
    }
    unsafe { runtime_ReentrantIterator_destroy(iter) };
    assert_eq!(got, vec![rt::MARSHAL_ERROR_CODE; 3]);
}

// ── Callback interfaces ─────────────────────────────────────────────────

/// The consumer's `Subscriber` vtable, laid out as the generated header
/// declares it: the header, then one nullable entry per method.
#[repr(C)]
pub struct SubscriberVtable {
    header: VtableHeader,
    on_message: Option<unsafe extern "C" fn(*mut c_void, Run, i32, Run, *mut FfiError) -> i64>,
    classify: Option<unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32>,
    on_ticker: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const bus::Ticker,
            *const bus::Ticker,
            *mut FfiError,
        ) -> u8,
    >,
    weigh: Option<unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i64>,
}

/// A consumer-side `Subscriber`: the context is a heap-allocated `SubState`
/// and the vtable a process-wide static, exactly as a binding does it.
mod consumer_subscriber {
    use super::*;
    use std::sync::atomic::AtomicI64;

    pub struct SubState {
        pub total: AtomicI64,
        pub fail_at: i32,
        pub last_topic: std::sync::Mutex<Option<String>>,
        pub freed: Arc<AtomicUsize>,
    }

    unsafe extern "C" fn on_message(
        ctx: *mut c_void,
        text: Run,
        weight: i32,
        envelope: Run,
        out_err: *mut FfiError,
    ) -> i64 {
        let state = unsafe { &*ctx.cast::<SubState>() };
        // Arguments are lent for the call: read them, don't free them.
        let text = unsafe { text.text() }.unwrap();
        let env: bus::Envelope =
            unsafe { rt::decode_run(envelope, |r| FfiType::<Tag>::read(r)) }.unwrap();
        *state.last_topic.lock().unwrap() = env.topic.clone();
        if weight == state.fail_at {
            fail(out_err, -1, &format!("subscriber rejected {text}"));
            return 0;
        }
        state.total.fetch_add(i64::from(weight), Ordering::Relaxed) + i64::from(weight)
    }

    unsafe extern "C" fn classify(_ctx: *mut c_void, weight: i32, _err: *mut FfiError) -> i32 {
        match weight {
            // An out-of-range return the producer can't accept.
            99 => 42,
            w => i32::from(w > 5),
        }
    }

    unsafe extern "C" fn on_ticker(
        _ctx: *mut c_void,
        ticker: *const bus::Ticker,
        alt: *const bus::Ticker,
        _err: *mut FfiError,
    ) -> u8 {
        // Object arguments move one strong reference: the consumer adopts
        // each non-null pointer and owes exactly one `_destroy`.
        let mut err = FfiError::default();
        unsafe {
            let v = runtime_Ticker_value(ticker, &mut err);
            runtime_Ticker_destroy(ticker.cast_mut());
            let alt_ok = if alt.is_null() {
                true
            } else {
                let same = runtime_Ticker_value(alt, &mut err) == v;
                runtime_Ticker_destroy(alt.cast_mut());
                same
            };
            match v {
                // A bool byte the producer can't accept.
                7 => 2,
                v => u8::from(v == 42 && alt_ok),
            }
        }
    }

    unsafe extern "C" fn weigh(ctx: *mut c_void, weight: i32, out_err: *mut FfiError) -> i64 {
        let state = unsafe { &*ctx.cast::<SubState>() };
        if weight == state.fail_at {
            fail(out_err, rt::FOREIGN_ERROR_CODE, "too heavy");
            return 0;
        }
        i64::from(weight) * 10
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        let state = unsafe { Box::from_raw(ctx.cast::<SubState>()) };
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    const fn vtable(size: usize) -> SubscriberVtable {
        SubscriberVtable {
            header: VtableHeader {
                size: size as u32,
                flags: 0,
                free: Some(free),
            },
            on_message: Some(on_message),
            classify: Some(classify),
            on_ticker: Some(on_ticker),
            weigh: Some(weigh),
        }
    }

    pub static VTABLE: SubscriberVtable = vtable(std::mem::size_of::<SubscriberVtable>());

    /// A binding generated when the interface had only `on_message`: its
    /// `size` ends after that entry.
    pub static SHORT_VTABLE: SubscriberVtable = vtable(
        std::mem::offset_of!(SubscriberVtable, on_message)
            + std::mem::size_of::<Option<unsafe extern "C" fn()>>(),
    );

    /// A binding that leaves `weigh` unimplemented (null).
    pub static NULL_SLOT_VTABLE: SubscriberVtable = SubscriberVtable {
        weigh: None,
        ..vtable(std::mem::size_of::<SubscriberVtable>())
    };

    /// A vtable whose `size` doesn't even cover its header.
    pub static TRUNCATED_VTABLE: SubscriberVtable = vtable(8);

    pub fn arg(
        vtable: &'static SubscriberVtable,
        fail_at: i32,
        freed: &Arc<AtomicUsize>,
    ) -> CallbackArg<SubscriberVtable> {
        CallbackArg {
            ctx: Box::into_raw(Box::new(SubState {
                total: AtomicI64::new(0),
                fail_at,
                last_topic: std::sync::Mutex::new(None),
                freed: Arc::clone(freed),
            }))
            .cast(),
            vtable,
        }
    }

    pub fn sub(fail_at: i32, freed: &Arc<AtomicUsize>) -> CallbackArg<SubscriberVtable> {
        arg(&VTABLE, fail_at, freed)
    }
}

#[test]
fn callback_interface_sync_paths() {
    use consumer_subscriber::sub;
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();

    // An enum return through a callback the call doesn't retain: `free` runs
    // as soon as the thunk drops its `Arc<dyn Subscriber>`.
    assert_eq!(
        unsafe { runtime_classify_once(sub(-1, &freed), 9, &mut err) },
        1
    );
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 1);

    // Objects move producer to consumer as owned references.
    assert_eq!(unsafe { runtime_tick(sub(-1, &freed), 42, &mut err) }, 1);
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 2);

    // A null vtable is a marshalling error, not a crash.
    let none = CallbackArg::<SubscriberVtable> {
        ctx: std::ptr::null_mut(),
        vtable: std::ptr::null(),
    };
    assert_eq!(unsafe { runtime_classify_once(none, 1, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert!(msg.contains("null"), "{msg}");
}

#[test]
fn callback_returns_the_producer_cant_accept_are_marshalling_failures() {
    use consumer_subscriber::sub;
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();

    // A bool return byte other than 0 or 1.
    let out = unsafe { runtime_tick_or_explain(sub(-1, &freed), 7, &mut err) };
    assert_eq!(
        take_string(out),
        "err -3: a callback interface returned an invalid value: 2 is not a valid bool"
    );
    // An enum return that names no variant.
    let out = unsafe { runtime_classify_or_explain(sub(-1, &freed), 99, &mut err) };
    assert_eq!(
        take_string(out),
        "err -3: a callback interface returned an invalid value: 42 is not a valid Priority"
    );
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 2);
}

#[test]
fn a_short_vtable_is_accepted_and_its_missing_methods_fail_with_minus_4() {
    use consumer_subscriber::{arg, NULL_SLOT_VTABLE, SHORT_VTABLE};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();

    // A method past the consumer's `size` fails without being called, and
    // the message names it.
    let out = unsafe { runtime_classify_or_explain(arg(&SHORT_VTABLE, -1, &freed), 9, &mut err) };
    assert_eq!(
        take_string(out),
        "err -4: the binding doesn't implement callback method `classify`; regenerate it"
    );
    let out = unsafe { runtime_weigh_or_explain(arg(&SHORT_VTABLE, -1, &freed), 9, &mut err) };
    assert!(take_string(out).contains("`weigh`"));

    // The methods the short vtable has still work.
    let b = unsafe { runtime_Bus_new(&mut err) };
    unsafe { runtime_Bus_subscribe(b, arg(&SHORT_VTABLE, -1, &freed), &mut err) };
    assert_eq!(unsafe { runtime_Bus_publish(b, text("x"), 4, &mut err) }, 4);
    assert_eq!(err.code, 0);
    unsafe { runtime_Bus_destroy(b.cast_mut()) };

    // A null entry is a method the binding doesn't implement, too.
    let out = unsafe { runtime_weigh_or_explain(arg(&NULL_SLOT_VTABLE, -1, &freed), 9, &mut err) };
    assert_eq!(
        take_string(out),
        "err -4: the binding doesn't implement callback method `weigh`; regenerate it"
    );

    // Propagated with `?`, the failure fails the producer's call.
    assert_eq!(
        unsafe { runtime_classify_once(arg(&SHORT_VTABLE, -1, &freed), 9, &mut err) },
        0
    );
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::GENERIC_ERROR_CODE, "`ForeignError` throws any");
    assert!(msg.contains("`classify`"), "{msg}");

    assert_eq!(
        freed.load(Ordering::SeqCst),
        5,
        "every context was released"
    );
}

#[test]
fn a_vtable_shorter_than_its_header_fails_the_call_with_minus_3() {
    use consumer_subscriber::{arg, TRUNCATED_VTABLE};
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();
    let cb = arg(&TRUNCATED_VTABLE, -1, &freed);
    assert_eq!(unsafe { runtime_classify_once(cb, 9, &mut err) }, 0);
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert_eq!(
        msg,
        "subscriber: a callback vtable is 8 bytes, shorter than its header"
    );
    // The header (and its `free`) couldn't be read, so the context is still
    // the consumer's.
    assert_eq!(freed.load(Ordering::SeqCst), 0);
    drop(unsafe { Box::from_raw(cb.ctx.cast::<consumer_subscriber::SubState>()) });
}

#[test]
fn callback_failures_return_as_values() {
    use consumer_subscriber::sub;
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();

    let out = unsafe { runtime_weigh_or_explain(sub(3, &freed), 2, &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(out), "ok 20");

    // The consumer fails, the producer sees an `Err`, and the call itself
    // succeeds: nothing unwound.
    let out = unsafe { runtime_weigh_or_explain(sub(3, &freed), 3, &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(out), "err -4: too heavy");
    assert_eq!(freed.load(Ordering::SeqCst), 2);
}

#[test]
fn callback_interface_retained_and_foreign_error() {
    use consumer_subscriber::sub;
    let freed = Arc::new(AtomicUsize::new(0));
    let mut err = FfiError::default();

    let b = unsafe { runtime_Bus_new(&mut err) };
    assert!(!b.is_null());
    unsafe {
        runtime_Bus_subscribe(b, sub(7, &freed), &mut err);
        runtime_Bus_subscribe(b, sub(-1, &freed), &mut err);
    }
    assert_eq!(err.code, 0);
    assert_eq!(freed.load(Ordering::SeqCst), 0, "retained by the bus");

    let publish = |weight: i32, err: &mut FfiError| unsafe {
        runtime_Bus_publish(b, text("hi"), weight, err)
    };
    assert_eq!(publish(3, &mut err), 6);
    assert_eq!(err.code, 0);
    assert_eq!(publish(5, &mut err), 16);

    // The first subscriber fails on weight 7: the producer call is aborted
    // and the consumer's own message comes back. `publish` throws `any`
    // (`ForeignError` isn't a domain), so the code is the generic one.
    assert_eq!(publish(7, &mut err), 0);
    assert_eq!(
        take_failure(&mut err),
        (rt::GENERIC_ERROR_CODE, "subscriber rejected hi".to_string())
    );

    // The bus is still usable afterwards.
    assert_eq!(publish(1, &mut err), 18);
    assert_eq!(err.code, 0);

    unsafe { runtime_Bus_clear(b, &mut err) };
    assert_eq!(freed.load(Ordering::SeqCst), 2, "free runs once each");
    unsafe { runtime_Bus_destroy(b.cast_mut()) };
}

/// The consumer's `Source` vtable.
#[repr(C)]
pub struct SourceVtable {
    header: VtableHeader,
    name: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> Run>,
    blob: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> Run>,
    card: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    token: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> *const rich::Token>,
    maybe_token: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> *const rich::Token>,
    lookup: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> i64>,
}

/// A consumer-side `Source`: returns of every family, allocated with the
/// exported `{prefix}_alloc`, and typed failures with a payload.
mod consumer_source {
    use super::*;

    unsafe extern "C" fn name(_ctx: *mut c_void, _err: *mut FfiError) -> Run {
        give(b"src")
    }

    unsafe extern "C" fn blob(_ctx: *mut c_void, _err: *mut FfiError) -> Run {
        give(&[1, 2])
    }

    unsafe extern "C" fn card(_ctx: *mut c_void, key: Run, _err: *mut FfiError) -> Run {
        let key = unsafe { key.text() }.unwrap();
        give(&encode(&rich::Card {
            name: format!("card-{key}"),
            tags: vec!["t".into()],
        }))
    }

    unsafe extern "C" fn token(_ctx: *mut c_void, _err: *mut FfiError) -> *const rich::Token {
        let mut err = FfiError::default();
        unsafe { runtime_Token_new(5, &mut err) }
    }

    unsafe extern "C" fn maybe_token(_ctx: *mut c_void, _err: *mut FfiError) -> *const rich::Token {
        std::ptr::null()
    }

    unsafe extern "C" fn lookup(_ctx: *mut c_void, key: Run, out_err: *mut FfiError) -> i64 {
        let key = unsafe { key.text() }.unwrap();
        let fail_with = |code: i32, payload: Option<&str>| {
            fail(out_err, code, &format!("failed {key}"));
            if let Some(p) = payload {
                let fields = encode(&p.to_string());
                unsafe { runtime_error_set_payload(out_err, fields.as_ptr(), fields.len()) };
            }
        };
        match key {
            "missing" => fail_with(1, Some("missing")),
            "busy" => fail_with(2, None),
            // A declared code whose payload doesn't decode.
            "garbled" => fail_with(1, None),
            "bogus" => fail_with(99, None),
            "trap" => fail_with(-1, None),
            _ => return 42,
        }
        0
    }

    pub static FREED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn free(_ctx: *mut c_void) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }

    pub static VTABLE: SourceVtable = SourceVtable {
        header: VtableHeader {
            size: std::mem::size_of::<SourceVtable>() as u32,
            flags: 0,
            free: Some(free),
        },
        name: Some(name),
        blob: Some(blob),
        card: Some(card),
        token: Some(token),
        maybe_token: Some(maybe_token),
        lookup: Some(lookup),
    };

    pub fn arg() -> CallbackArg<SourceVtable> {
        CallbackArg {
            ctx: std::ptr::null_mut(),
            vtable: &VTABLE,
        }
    }
}

#[test]
fn callback_returns_of_every_family_are_adopted() {
    let mut err = FfiError::default();
    let out = unsafe { runtime_describe_source(consumer_source::arg(), &mut err) };
    assert_eq!(err.code, 0, "{}", message(&err));
    assert_eq!(take_string(out), r#"src [1, 2] card-k:["t"] 5 false"#);
}

#[test]
fn throwing_callbacks_report_typed_domain_errors() {
    let lookup = |key: &str| {
        let mut err = FfiError::default();
        let out = unsafe { runtime_lookup_via(consumer_source::arg(), text(key), &mut err) };
        assert_eq!(err.code, 0);
        take_string(out)
    };
    assert_eq!(lookup("a"), "ok 42");
    assert_eq!(
        lookup("missing"),
        "missing missing",
        "code 1 with its payload"
    );
    assert_eq!(lookup("busy"), "busy");
    // A declared code without its payload, an undeclared positive code, or
    // any negative one is a foreign failure.
    assert_eq!(lookup("garbled"), "foreign -4: failed garbled");
    assert_eq!(lookup("bogus"), "foreign -4: failed bogus");
    assert_eq!(lookup("trap"), "foreign -4: failed trap");
}

#[test]
fn optional_callback_parameters_accept_null() {
    let mut err = FfiError::default();
    let none = CallbackArg::<SourceVtable> {
        ctx: std::ptr::null_mut(),
        vtable: std::ptr::null(),
    };
    assert_eq!(
        take_string(unsafe { runtime_has_source(none, text("x"), &mut err) }),
        "x:false"
    );
    assert_eq!(
        take_string(unsafe { runtime_has_source(consumer_source::arg(), text("x"), &mut err) }),
        "x:true"
    );
    assert_eq!(unsafe { runtime_always(&mut err) }, 1);
}

// ── Async ───────────────────────────────────────────────────────────────

type Completion<T> = mpsc::Sender<(i32, String, T)>;

/// Box a sender as an async `context`; reclaim it with [`drop_ctx`].
fn new_ctx<T>(tx: Completion<T>) -> *mut c_void {
    Box::into_raw(Box::new(tx)).cast()
}

fn drop_ctx<T>(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<Completion<T>>()) });
}

/// Copy a consumer-owned async error and release it with `error_free`.
fn take_async_err(err: *mut FfiError) -> (i32, String) {
    if err.is_null() {
        return (0, String::new());
    }
    let out = unsafe { ((*err).code, message(&*err)) };
    unsafe { runtime_error_free(err) };
    out
}

fn send<T>(ctx: *mut c_void, err: *mut FfiError, value: T) {
    // Clone the sender before sending: the test may free the context as
    // soon as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<Completion<T>>() }.clone();
    let (code, msg) = take_async_err(err);
    tx.send((code, msg, value)).unwrap();
}

unsafe extern "C" fn on_i64(ctx: *mut c_void, err: *mut FfiError, result: i64) {
    send(ctx, err, result);
}

unsafe extern "C" fn on_i32(ctx: *mut c_void, err: *mut FfiError, result: i32) {
    send(ctx, err, result);
}

unsafe extern "C" fn on_string(ctx: *mut c_void, err: *mut FfiError, result: Run) {
    let value = if result.ptr.is_null() {
        String::new()
    } else {
        take_string(result)
    };
    send(ctx, err, value);
}

unsafe extern "C" fn on_unit(ctx: *mut c_void, err: *mut FfiError) {
    send(ctx, err, ());
}

#[test]
fn callback_interface_from_an_async_method() {
    use consumer_subscriber::sub;
    let mut err = FfiError::default();
    let b = unsafe { runtime_Bus_new(&mut err) };
    let freed = Arc::new(AtomicUsize::new(0));
    unsafe { runtime_Bus_subscribe(b, sub(4, &freed), &mut err) };

    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i64>(tx);
    unsafe { runtime_Bus_publish_later(b, text("async"), 2, Some(on_i64), ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 2));

    // A foreign failure inside the future is delivered through the
    // completion, with the zero result.
    unsafe { runtime_Bus_publish_later(b, text("async"), 4, Some(on_i64), ctx) };
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (
            rt::GENERIC_ERROR_CODE,
            "subscriber rejected async".to_string(),
            0
        )
    );

    // The receiver was retained by the launcher: releasing the consumer's
    // reference while a call is in flight is safe.
    unsafe {
        runtime_Bus_publish_later(b, text("async"), 1, Some(on_i64), ctx);
        runtime_Bus_destroy(b.cast_mut());
    }
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 3));
    drop_ctx::<i64>(ctx);
}

#[test]
fn async_record_result_completes_via_the_callback() {
    unsafe extern "C" fn on_task(ctx: *mut c_void, err: *mut FfiError, result: Run) {
        let value = (!result.ptr.is_null()).then(|| take::<tasks::TaskResult>(result));
        send(ctx, err, value);
    }
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<Option<tasks::TaskResult>>(tx);
    unsafe { runtime_run_task(text("alpha"), Some(on_task), ctx) };
    let (code, _, value) = rx.recv_timeout(WAIT).unwrap();
    drop_ctx::<Option<tasks::TaskResult>>(ctx);
    assert_eq!(code, 0);
    assert_eq!(
        value,
        Some(tasks::TaskResult {
            id: 7,
            value: "done: alpha".to_string()
        })
    );
}

#[test]
fn async_string_result_is_one_run() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<String>(tx);
    // The launcher copies the borrowed argument before returning, so the
    // consumer may release it right away.
    let arg = "quiet\0please".to_string();
    unsafe { runtime_shout(text(&arg), Some(on_string), ctx) };
    drop(arg);
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (0, String::new(), "QUIET\0PLEASE".into())
    );

    // An argument that doesn't lift is reported through the completion.
    unsafe { runtime_shout(Run::borrowing(&[0xff]), Some(on_string), ctx) };
    let (code, _, value) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!((code, value.as_str()), (rt::MARSHAL_ERROR_CODE, ""));
    drop_ctx::<String>(ctx);
}

#[test]
fn async_result_ok_and_err_paths() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    unsafe { runtime_checked_add(2, 3, Some(on_i32), ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 5));
    unsafe { runtime_checked_add(i32::MAX, 1, Some(on_i32), ctx) };
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (1, "arithmetic overflow".to_string(), 0)
    );
    drop_ctx::<i32>(ctx);
}

#[test]
fn async_unit_completion_has_no_result_slot() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<()>(tx);
    unsafe { runtime_tidy(0, Some(on_unit), ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), ()));
    unsafe { runtime_tidy(1, Some(on_unit), ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap().0, 1);
    unsafe { runtime_tidy(2, Some(on_unit), ctx) };
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap().0,
        rt::MARSHAL_ERROR_CODE,
        "a bad bool"
    );
    drop_ctx::<()>(ctx);
}

#[test]
fn a_null_completion_discards_the_result() {
    unsafe {
        runtime_checked_add(1, 2, None, std::ptr::null_mut());
        runtime_checked_add(i32::MAX, 1, None, std::ptr::null_mut());
        runtime_shout(text("x"), None, std::ptr::null_mut());
    }
    // Nothing to observe but that nothing crashed or leaked (see `leak.rs`);
    // make sure the executor ran them before the binary exits.
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    unsafe { runtime_checked_add(0, 0, Some(on_i32), ctx) };
    rx.recv_timeout(WAIT).unwrap();
    drop_ctx::<i32>(ctx);
}

#[test]
fn cancelling_completes_with_the_cancelled_code_exactly_once() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    let token = unsafe { runtime_cancel_token_create() };
    unsafe { runtime_wait_forever(token, Some(on_i32), ctx) };
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "not complete until cancelled"
    );
    unsafe {
        runtime_cancel_token_cancel(token);
        assert!(runtime_cancel_token_is_cancelled(token));
        runtime_cancel_token_destroy(token);
    }
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        (rt::CANCELLED_ERROR_CODE, "cancelled".to_string(), 0)
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
        let token = unsafe { runtime_cancel_token_create() };
        if i % 2 == 0 {
            // The launcher takes its own reference before returning, so the
            // consumer may then cancel and destroy its reference from
            // another thread while the spawned future starts running.
            unsafe { runtime_wait_forever(token, Some(on_i32), ctx) };
            let raw = token as usize;
            std::thread::spawn(move || unsafe {
                runtime_cancel_token_cancel(raw as *mut FfiCancelToken);
                runtime_cancel_token_destroy(raw as *mut FfiCancelToken);
            })
            .join()
            .unwrap();
        } else {
            // Cancel and destroy before the spawned future is ever polled.
            unsafe {
                runtime_wait_forever(token, Some(on_i32), ctx);
                runtime_cancel_token_cancel(token);
                runtime_cancel_token_destroy(token);
            }
        }
    }
    for _ in 0..RUNS {
        assert_eq!(rx.recv_timeout(WAIT).unwrap().0, rt::CANCELLED_ERROR_CODE);
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "each call completed exactly once"
    );
    drop_ctx::<i32>(ctx);
}

#[test]
fn a_cancelled_token_cancels_at_launch_and_a_null_one_never_does() {
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i32>(tx);
    let token = unsafe { runtime_cancel_token_create() };
    unsafe {
        runtime_cancel_token_cancel(token);
        runtime_wait_forever(token, Some(on_i32), ctx);
        runtime_cancel_token_destroy(token);
    }
    assert_eq!(rx.recv_timeout(WAIT).unwrap().0, rt::CANCELLED_ERROR_CODE);
    drop_ctx::<i32>(ctx);
}

#[cfg(feature = "tokio")]
#[test]
fn many_concurrent_launches_share_a_few_executor_threads() {
    const CALLS: usize = 64;
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<String>(tx);
    for _ in 0..CALLS {
        unsafe { runtime_thread_tag(Some(on_string), ctx) };
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

/// Without Tokio (`default-features = false`), each call runs on a thread
/// of its own.
#[cfg(not(feature = "tokio"))]
#[test]
fn without_tokio_each_launch_gets_its_own_thread() {
    const CALLS: usize = 16;
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<String>(tx);
    for _ in 0..CALLS {
        unsafe { runtime_thread_tag(Some(on_string), ctx) };
    }
    let mut threads = std::collections::BTreeSet::new();
    for _ in 0..CALLS {
        let (code, _, tag) = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(code, 0);
        assert!(tag.ends_with(" weaveffi-async"), "{tag}");
        threads.insert(tag);
    }
    drop_ctx::<String>(ctx);
    assert_eq!(threads.len(), CALLS, "one thread per call");
}

// ── Objects ─────────────────────────────────────────────────────────────

#[test]
fn borrowed_and_optional_objects_from_another_module() {
    let mut err = FfiError::default();
    let session = unsafe { runtime_Session_open(7, &mut err) };
    assert_eq!(err.code, 0);
    assert!(!session.is_null());

    let report: outer::inner::Report =
        take(unsafe { runtime_summarize(session, std::ptr::null(), &mut err) });
    assert_eq!(err.code, 0);
    assert_eq!(report.score, 70);
    assert!(report.session.is_none());
    let report: outer::inner::Report =
        take(unsafe { runtime_summarize(session, session, &mut err) });
    assert_eq!(report.score, 77);

    // A null required object is a marshalling failure.
    let out = unsafe { runtime_summarize(std::ptr::null(), session, &mut err) };
    assert_eq!(out, Run::EMPTY);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    assert_eq!(unsafe { runtime_raise(1, &mut err) }, 2);
    unsafe { runtime_Session_destroy(session.cast_mut()) };
}

#[test]
fn object_reference_counting() {
    let mut err = FfiError::default();
    unsafe {
        let s = runtime_Session_open(3, &mut err);

        // `share` retains through `Arc<Session>` in and hands back a new
        // strong reference to the same allocation.
        let again = runtime_share(s, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(again, s, "the same object, one more reference");
        let third = runtime_Session_clone(s);
        assert_eq!(third, s);

        runtime_Session_destroy(s.cast_mut());
        runtime_Session_destroy(again.cast_mut());
        // Still alive through `third`.
        let report: outer::inner::Report =
            take(runtime_summarize(third, std::ptr::null(), &mut err));
        assert_eq!(report.score, 30);
        runtime_Session_destroy(third.cast_mut());
        runtime_Session_destroy(std::ptr::null_mut());
        assert!(runtime_Session_clone(std::ptr::null()).is_null());
    }
}

#[test]
fn objects_inside_value_buffers_carry_a_reference() {
    let mut err = FfiError::default();
    unsafe {
        let s = runtime_Session_open(5, &mut err);

        // `attach` retains the session inside the returned record: the
        // buffer's object token is one strong reference the consumer owns.
        let run = runtime_attach(s, &mut err);
        assert_eq!(err.code, 0);
        let bytes = std::slice::from_raw_parts(run.ptr, run.len).to_vec();
        runtime_run_free(run);
        runtime_Session_destroy(s.cast_mut());

        // Sending the buffer back moves the token's reference to the
        // producer, which returns it as the optional object result.
        let back = runtime_session_of(Run::borrowing(&bytes), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(back, s, "same allocation, still alive");
        runtime_Session_destroy(back.cast_mut());

        let none = encode(&outer::inner::Report {
            score: 0,
            session: None,
        });
        let back = runtime_session_of(Run::borrowing(&none), &mut err);
        assert!(back.is_null());
        assert_eq!(err.code, 0, "a null optional object return isn't an error");
    }
}

#[test]
fn interface_constructor_methods_destroy() {
    let mut err = FfiError::default();
    unsafe {
        let c = runtime_Counter_new(10, &mut err);
        assert_eq!(err.code, 0);
        assert!(!c.is_null());
        assert_eq!(runtime_Counter_increment(c, &mut err), 11);
        assert_eq!(runtime_Counter_increment(c, &mut err), 12);
        assert_eq!(runtime_Counter_value(c, &mut err), 12);
        assert_eq!(err.code, 0);
        assert_eq!(
            take_string(runtime_Counter_label(c, text("n="), &mut err)),
            "n=12"
        );
        runtime_Counter_destroy(c.cast_mut());
    }
}

#[test]
fn interface_fallible_constructor() {
    let mut err = FfiError::default();
    unsafe {
        let ok = runtime_Counter_with_step(0, 5, &mut err);
        assert_eq!(err.code, 0);
        assert!(!ok.is_null());
        assert_eq!(runtime_Counter_increment(ok, &mut err), 5);
        runtime_Counter_destroy(ok.cast_mut());

        let bad = runtime_Counter_with_step(0, 0, &mut err);
        assert!(bad.is_null());
        assert_eq!(
            take_failure(&mut err),
            (1, "step must be positive".to_string())
        );
    }
}

#[test]
fn interface_returning_method_and_static() {
    let mut err = FfiError::default();
    unsafe {
        assert_eq!(runtime_Counter_default_start(&mut err), 0);
        let c = runtime_Counter_new(3, &mut err);
        let snap = runtime_Counter_snapshot(c, &mut err);
        assert!(!snap.is_null());
        runtime_Counter_increment(c, &mut err);
        assert_eq!(runtime_Counter_value(c, &mut err), 4);
        assert_eq!(
            runtime_Counter_value(snap, &mut err),
            3,
            "the snapshot is an independent object"
        );
        runtime_Counter_destroy(snap.cast_mut());
        runtime_Counter_destroy(c.cast_mut());
    }
}

#[test]
fn interface_as_free_function_parameter() {
    let mut err = FfiError::default();
    unsafe {
        let c = runtime_Counter_new(21, &mut err);
        assert_eq!(runtime_read_twice(c, &mut err), 42);
        runtime_Counter_destroy(c.cast_mut());
    }
}

#[test]
fn arc_self_receiver_and_optional_objects() {
    let mut err = FfiError::default();
    unsafe {
        let c = runtime_Counter_new(10, &mut err);
        let shared = runtime_Counter_share(c, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(shared, c, "`self: Arc<Self>` returns the same object");
        let third = runtime_Counter_clone(c);
        runtime_Counter_destroy(shared.cast_mut());
        runtime_Counter_destroy(third.cast_mut());

        let other = runtime_Counter_new(20, &mut err);
        let bigger = runtime_Counter_larger(c, other, 0, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(runtime_Counter_value(bigger, &mut err), 20);
        runtime_Counter_destroy(bigger.cast_mut());

        let mine = runtime_Counter_larger(c, std::ptr::null(), 0, &mut err);
        assert_eq!(runtime_Counter_value(mine, &mut err), 10);
        runtime_Counter_destroy(mine.cast_mut());

        let none = runtime_Counter_larger(c, other, 100, &mut err);
        assert!(none.is_null());
        assert_eq!(err.code, 0, "a null optional object return isn't an error");

        runtime_Counter_destroy(other.cast_mut());
        runtime_Counter_destroy(c.cast_mut());
    }
}

#[test]
fn iterator_of_objects() {
    let mut err = FfiError::default();
    let mut values = Vec::new();
    unsafe {
        let c = runtime_Counter_new(5, &mut err);
        let iter = runtime_Counter_fan_out(c, 3, &mut err);
        assert_eq!(err.code, 0);
        let mut item: *const counters::Counter = std::ptr::null();
        while runtime_Counter_FanOutIterator_next(iter, &mut item, &mut err) == 1 {
            values.push(runtime_Counter_value(item, &mut err));
            runtime_Counter_destroy(item.cast_mut());
        }
        runtime_Counter_FanOutIterator_destroy(iter);
        runtime_Counter_destroy(c.cast_mut());
    }
    assert_eq!(values, [5, 6, 7]);
}

#[test]
fn async_methods_retain_the_receiver() {
    unsafe extern "C" fn on_obj(
        ctx: *mut c_void,
        err: *mut FfiError,
        result: *const counters::Counter,
    ) {
        let mut e = FfiError::default();
        let v = unsafe {
            let v = runtime_Counter_value(result, &mut e);
            runtime_Counter_destroy(result.cast_mut());
            v
        };
        send(ctx, err, v);
    }

    let mut err = FfiError::default();
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i64>(tx);
    unsafe {
        let c = runtime_Counter_new(8, &mut err);
        runtime_Counter_value_later(c, Some(on_i64), ctx);
        runtime_Counter_snapshot_later(c, Some(on_obj), ctx);
        // Releasing the consumer's reference while calls are in flight is
        // safe: each launcher retained its own.
        runtime_Counter_destroy(c.cast_mut());
    }
    let mut got = [
        rx.recv_timeout(WAIT).unwrap().2,
        rx.recv_timeout(WAIT).unwrap().2,
    ];
    got.sort_unstable();
    assert_eq!(got, [8, 8]);

    // A null receiver still completes (with a marshalling error).
    unsafe { runtime_Counter_value_later(std::ptr::null(), Some(on_i64), ctx) };
    let (code, _, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!((code, result), (rt::MARSHAL_ERROR_CODE, 0));
    drop_ctx::<i64>(ctx);
}

#[test]
fn an_async_factory_completes_with_a_new_object() {
    unsafe extern "C" fn on_obj(
        ctx: *mut c_void,
        err: *mut FfiError,
        result: *const counters::Counter,
    ) {
        let mut e = FfiError::default();
        let v = unsafe {
            let v = runtime_Counter_value(result, &mut e);
            runtime_Counter_destroy(result.cast_mut());
            v
        };
        send(ctx, err, v);
    }
    let (tx, rx) = mpsc::channel();
    let ctx = new_ctx::<i64>(tx);
    unsafe { runtime_Counter_start_later(12, Some(on_obj), ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, String::new(), 12));
    drop_ctx::<i64>(ctx);
}

#[test]
fn interface_null_self_reports_an_error() {
    let mut err = FfiError::default();
    assert_eq!(
        unsafe { runtime_Counter_value(std::ptr::null(), &mut err) },
        0
    );
    let (code, msg) = take_failure(&mut err);
    assert_eq!(code, rt::MARSHAL_ERROR_CODE);
    assert_eq!(msg, "self: an object is null");
}

#[test]
fn producer_panic_reports_the_panic_code() {
    let mut err = FfiError::default();
    unsafe {
        let c = runtime_Counter_new(0, &mut err);
        runtime_Counter_explode(c, &mut err);
        let (code, msg) = take_failure(&mut err);
        assert_eq!(code, rt::PANIC_ERROR_CODE);
        assert!(msg.contains("counter exploded"), "{msg}");

        // The object is still usable.
        assert_eq!(runtime_Counter_value(c, &mut err), 0);
        assert_eq!(err.code, 0);
        runtime_Counter_destroy(c.cast_mut());
    }
}

#[test]
fn skipped_and_private_members_are_ordinary_rust() {
    let c = counters::Counter::new(1);
    assert_eq!(c.internal()(), -1);
}

// ── Errors ──────────────────────────────────────────────────────────────

#[test]
fn fallible_with_domain_error_codes() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_fetch(21, &mut err) }, 42);
    assert_eq!(err.code, 0);

    assert_eq!(unsafe { runtime_fetch(0, &mut err) }, 0);
    assert_eq!(
        take_failure(&mut err),
        (2001, "entry not found".to_string())
    );

    assert_eq!(unsafe { runtime_fetch(-1, &mut err) }, 0);
    assert_eq!(err.payload(), encode(&"negative key".to_string()));
    assert_eq!(
        take_failure(&mut err),
        (2002, "vault sealed: negative key".to_string())
    );
}

#[test]
fn untyped_errors_report_the_generic_code_and_display() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_fetch_any(5, &mut err) }, 5);
    assert_eq!(unsafe { runtime_fetch_any(0, &mut err) }, 0);
    assert!(err.payload_ptr.is_null());
    assert_eq!(
        take_failure(&mut err),
        (rt::GENERIC_ERROR_CODE, "vault failure: no key".to_string())
    );
    assert_eq!(unsafe { runtime_parse_int(text("x1"), &mut err) }, 0);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::GENERIC_ERROR_CODE,
            "invalid digit found in string".to_string()
        )
    );
    unsafe { runtime_refuse(text("no"), &mut err) };
    assert_eq!(
        take_failure(&mut err),
        (rt::GENERIC_ERROR_CODE, "no".to_string())
    );
}

#[test]
fn error_payload_fields_cross_the_abi() {
    let mut err = FfiError::default();

    // Success leaves the payload slots empty.
    assert_eq!(unsafe { runtime_consume(30, &mut err) }, 70);
    assert_eq!(err.code, 0);
    assert!(err.payload_ptr.is_null());

    // A payload-carrying code encodes its fields in declaration order.
    assert_eq!(unsafe { runtime_consume(250, &mut err) }, 0);
    assert_eq!(err.code, 3001);
    assert_eq!(message(&err), "used 250 of 100");
    let mut reader = rt::BufferReader::new(err.payload());
    assert_eq!(reader.read_i64().unwrap(), 100, "limit field");
    assert_eq!(reader.read_i64().unwrap(), 250, "used field");
    reader.expect_end().unwrap();
    unsafe { runtime_error_clear(&mut err) };
    assert!(err.payload_ptr.is_null(), "clear releases the payload");

    // A unit code reports a code and message with no payload.
    assert_eq!(unsafe { runtime_consume(-1, &mut err) }, 0);
    assert!(err.payload_ptr.is_null());
    assert_eq!(take_failure(&mut err).0, 3002);
}

#[test]
fn deprecated_export_thunk_compiles_and_runs() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { runtime_add_one(41, &mut err) }, 42);
    assert_eq!(unsafe { runtime_bump(41, &mut err) }, 42);
    assert_eq!(err.code, 0);
}

// ── The contract ────────────────────────────────────────────────────────

/// Declare each contract function by its symbol and list it with the
/// declaration path whose id the symbol encodes.
macro_rules! contracts {
    ($($path:literal => $symbol:ident;)*) => {
        unsafe extern "C" {
            $(fn $symbol() -> u64;)*
        }

        /// Every contract function this test checks: its path, its symbol,
        /// and the function.
        const CONTRACTS: &[(&str, &str, unsafe extern "C" fn() -> u64)] =
            &[$(($path, stringify!($symbol), $symbol),)*];
    };
}

contracts! {
    "add" => runtime_contract_e70bc3190530e654;
    "checked_div" => runtime_contract_23bafa60563adf2e;
    "greet" => runtime_contract_0f40ea29637fe28a;
    "char_count" => runtime_contract_888e76bc7ab24475;
    "maybe_name" => runtime_contract_206c57294840c577;
    "sum" => runtime_contract_82719e195d0fc4a8;
    "join" => runtime_contract_41def2de29651139;
    "byte_count" => runtime_contract_8906c210ed6818ef;
    "make_point" => runtime_contract_1283be904bcc1566;
    "point_x" => runtime_contract_5baaad07b623a796;
    "echo_color" => runtime_contract_a1a734a46207b124;
    "negate_opt" => runtime_contract_2411cd574dabd4cd;
    "next_color" => runtime_contract_35fb39dea4228f08;
    "describe_shape" => runtime_contract_69e6b706387abcd6;
    "double_scores" => runtime_contract_fd74192e01224262;
    "greetings" => runtime_contract_4c3c384e13d73fff;
    "lookup_via" => runtime_contract_1bfa4634e400debc;
    "has_source" => runtime_contract_39242963ed799e71;
    "checked_add" => runtime_contract_f87d3f603dc27d4e;
    "wait_forever" => runtime_contract_87213461b41707e0;
    "tidy" => runtime_contract_1e6b95ef2eb28d95;
    "shout" => runtime_contract_f6a25348f7903d22;
    "fetch_any" => runtime_contract_79c8f5129aec9e3e;
    "summarize" => runtime_contract_20ea7db5ad4182da;
    "DemoError" => runtime_contract_ddabffd3b2836960;
    "DemoError.DivisionByZero" => runtime_contract_ad05ffc62f5172f6;
    "Color" => runtime_contract_344ea8f4939cdc98;
    "Point" => runtime_contract_8a439296ced9ed11;
    "Shape" => runtime_contract_2de969afacdaa7a6;
    "Card" => runtime_contract_dfef4f9ce9ebb93f;
    "Token" => runtime_contract_e68398e3c2c760b2;
    "Token.new" => runtime_contract_19f363a91a374252;
    "Token.n" => runtime_contract_5fbf85bfbdd17a4e;
    "Source" => runtime_contract_5f43af3669b1e098;
    "Source.name" => runtime_contract_099a038fbbc6a0c9;
    "Source.blob" => runtime_contract_be2e792aacbbe789;
    "Source.card" => runtime_contract_6e086a3672527db8;
    "Source.token" => runtime_contract_4a0d3e3776320b37;
    "Source.maybe_token" => runtime_contract_5e5727760cdeadb0;
    "Source.lookup" => runtime_contract_f81e191ed9aa28e4;
    "LookupError" => runtime_contract_4bb33b369eb5bfb3;
    "LookupError.Missing" => runtime_contract_77d77b01e74db5e5;
    "LookupError.Busy" => runtime_contract_0408a315f2eb68ea;
    "LookupError.Foreign" => runtime_contract_e4757f97fe182aa5;
    "Counter" => runtime_contract_9d1c372fe94004c3;
    "Counter.new" => runtime_contract_b183fbbf6b26d7d7;
    "Counter.with_step" => runtime_contract_53d4008d5e865122;
    "Counter.increment" => runtime_contract_9c7999dc7298c42c;
    "Counter.value" => runtime_contract_7f1ac9b2a453892c;
    "Counter.label" => runtime_contract_5fecc32bfb44a1af;
    "Counter.snapshot" => runtime_contract_f0445856c125fccf;
    "Counter.share" => runtime_contract_a97f3234ad78ef4e;
    "Counter.larger" => runtime_contract_08023540153f2b40;
    "Counter.fan_out" => runtime_contract_15af951b2b8296bf;
    "Counter.value_later" => runtime_contract_258121755f9bd75d;
    "Counter.snapshot_later" => runtime_contract_99fe96b5a27dd3fa;
    "Counter.explode" => runtime_contract_6f4108f7392032a8;
    "Counter.default_start" => runtime_contract_0dce532b6a3460d7;
    "Counter.start_later" => runtime_contract_ecc43c00e61c8c58;
    "CounterError" => runtime_contract_863c2982fa2e56cb;
    "CounterError.OutOfRange" => runtime_contract_9b4a1aa0d9e3e959;
    "Session" => runtime_contract_62278f512692e037;
    "Session.open" => runtime_contract_150044d742122907;
    "Report" => runtime_contract_9b6b1422027158eb;
    "TaskError" => runtime_contract_958a5b3ae3901fba;
    "TaskError.Overflow" => runtime_contract_3cfe66cdbc5f7eca;
    "QuotaError" => runtime_contract_55e719d8930b2933;
    "QuotaError.Exceeded" => runtime_contract_0d6e025b63a2f0ca;
    "QuotaError.Unavailable" => runtime_contract_4700d6d1c46ed573;
}

/// The checked part of this library's API in the IDL: what `weaveffi
/// generate` assembles from the metadata the attributes embed.
const IDL: &str = r#"
version: "0.13.0"
errors:
  - name: DemoError
    codes:
      - { name: DivisionByZero, code: 100, message: "cannot divide by zero" }
  - name: LookupError
    codes:
      - { name: Missing, code: 1, message: "missing {key}", fields: [{ name: key, type: string }] }
      - { name: Busy, code: 2, message: busy }
      - name: Foreign
        code: 3
        message: "foreign {code}: {message}"
        fields: [{ name: code, type: i32 }, { name: message, type: string }]
  - name: CounterError
    codes:
      - { name: OutOfRange, code: 1, message: "step must be positive" }
  - name: TaskError
    codes:
      - { name: Overflow, code: 1, message: "arithmetic overflow" }
  - name: QuotaError
    codes:
      - name: Exceeded
        code: 3001
        message: "used {used} of {limit}"
        fields: [{ name: limit, type: i64 }, { name: used, type: i64 }]
      - { name: Unavailable, code: 3002, message: "quota service unavailable" }
enums:
  - name: Color
    variants: [{ name: Red, value: 0 }, { name: Green, value: 1 }, { name: Blue, value: 2 }]
  - name: Shape
    variants:
      - { name: Empty, value: 0 }
      - { name: Circle, value: 1, fields: [{ name: radius, type: f64 }] }
      - { name: Labeled, value: 2, fields: [{ name: label, type: string }, { name: count, type: u8 }] }
records:
  - name: Point
    fields:
      - { name: x, type: i32 }
      - { name: label, type: string }
      - { name: nickname, type: "string?" }
      - { name: color, type: Color }
  - name: Card
    fields: [{ name: name, type: string }, { name: tags, type: "[string]" }]
  - name: Report
    fields: [{ name: score, type: i64 }, { name: session, type: "Session?" }]
interfaces:
  - name: Token
    constructors: [{ name: new, params: [{ name: n, type: i64 }] }]
    methods: [{ name: n, return: i64 }]
  - name: Counter
    constructors:
      - { name: new, params: [{ name: start, type: i64 }] }
      - name: with_step
        params: [{ name: start, type: i64 }, { name: step, type: i64 }]
        throws: CounterError
    methods:
      - { name: increment, return: i64 }
      - { name: value, return: i64 }
      - { name: label, params: [{ name: prefix, type: string }], return: string }
      - { name: snapshot, return: Counter }
      - { name: share, return: Counter }
      - name: larger
        params: [{ name: other, type: "Counter?" }, { name: floor, type: i64 }]
        return: "Counter?"
      - { name: fan_out, params: [{ name: n, type: i32 }], return: "iter<Counter>" }
      - { name: value_later, return: i64, async: true }
      - { name: snapshot_later, return: Counter, async: true }
      - { name: explode }
    statics:
      - { name: default_start, return: i64 }
      - { name: start_later, params: [{ name: start, type: i64 }], return: Counter, async: true }
  - name: Session
    constructors: [{ name: open, params: [{ name: id, type: i64 }] }]
callbacks:
  - name: Source
    methods:
      - { name: name, return: string, throws: any }
      - { name: blob, return: bytes, throws: any }
      - { name: card, params: [{ name: key, type: string }], return: Card, throws: any }
      - { name: token, return: Token, throws: any }
      - { name: maybe_token, return: "Token?", throws: any }
      - { name: lookup, params: [{ name: key, type: string }], return: i64, throws: LookupError }
functions:
  - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
  - name: checked_div
    params: [{ name: a, type: i32 }, { name: b, type: i32 }]
    return: i32
    throws: DemoError
  - { name: greet, params: [{ name: name, type: string }], return: string }
  - { name: char_count, params: [{ name: text, type: string }], return: i32 }
  - { name: maybe_name, params: [{ name: present, type: bool }], return: "string?" }
  - { name: sum, params: [{ name: xs, type: "[i32]" }], return: i32 }
  - { name: join, params: [{ name: parts, type: "[string]" }], return: string }
  - { name: byte_count, params: [{ name: data, type: bytes }], return: i32 }
  - { name: make_point, params: [{ name: x, type: i32 }], return: Point }
  - { name: point_x, params: [{ name: p, type: Point }], return: i32 }
  - { name: echo_color, params: [{ name: c, type: Color }], return: Color }
  - { name: negate_opt, params: [{ name: flag, type: "bool?" }], return: "bool?" }
  - { name: next_color, params: [{ name: color, type: "Color?" }], return: "Color?" }
  - { name: describe_shape, params: [{ name: shape, type: Shape }], return: string }
  - name: double_scores
    params: [{ name: scores, type: "{string:i32}" }]
    return: "{string:i32}"
  - { name: greetings, params: [{ name: count, type: i32 }], return: "iter<string>" }
  - { name: lookup_via, params: [{ name: source, type: Source }, { name: key, type: string }], return: string }
  - name: has_source
    params: [{ name: source, type: "Source?" }, { name: label, type: string }]
    return: string
  - name: checked_add
    params: [{ name: a, type: i32 }, { name: b, type: i32 }]
    return: i32
    throws: TaskError
    async: true
  - { name: wait_forever, return: i32, async: true, cancellable: true }
  - { name: tidy, params: [{ name: fail, type: bool }], throws: TaskError, async: true }
  - { name: shout, params: [{ name: text, type: string }], return: string, async: true }
  - { name: fetch_any, params: [{ name: key, type: i64 }], return: i64, throws: any }
  - name: summarize
    params: [{ name: session, type: Session }, { name: keep, type: "Session?" }]
    return: Report
"#;

#[test]
fn every_declaration_exports_the_contract_the_model_computes() {
    use weaveffi_model::contract;

    let api = weaveffi_model::parse::parse_api_str(IDL, "yaml").expect("the IDL parses");
    let model = weaveffi_model::validate::validate(
        &api,
        &weaveffi_model::pkg::Identity::named("runtime"),
        None,
    )
    .expect("the IDL validates");
    let entries = contract::entries(&model);
    assert_eq!(
        entries.len(),
        CONTRACTS.len(),
        "the table checks every entry"
    );
    for entry in &entries {
        let (_, symbol, f) = CONTRACTS
            .iter()
            .find(|(path, ..)| *path == entry.path)
            .unwrap_or_else(|| panic!("no contract function listed for `{}`", entry.path));
        assert_eq!(
            *symbol, entry.symbol,
            "`{}` is exported under its id",
            entry.path
        );
        assert_eq!(
            unsafe { f() },
            entry.hash,
            "`{}`: the producer's signature differs from `{}`",
            entry.path,
            entry.signature
        );
    }
}

#[test]
fn contract_signatures_are_the_canonical_strings() {
    // A few spelled out by hand, so the test above can't pass by the model
    // and the macros agreeing on a wrong spelling.
    let hash = |path: &str| {
        let (.., f) = CONTRACTS.iter().find(|(p, ..)| *p == path).unwrap();
        unsafe { f() }
    };
    let fnv = |s: &str| weaveffi_model::contract::fnv1a64(s.as_bytes());
    assert_eq!(hash("add"), fnv("function add(i32, i32) -> i32"));
    assert_eq!(
        hash("lookup_via"),
        fnv("function lookup_via(Source, string) -> string"),
        "the alias resolved to its target"
    );
    assert_eq!(
        hash("wait_forever"),
        fnv("function wait_forever() -> i32 async cancellable")
    );
    assert_eq!(
        hash("Point"),
        fnv("record Point {i32, string, string?, Color}")
    );
    assert_eq!(
        hash("Shape"),
        fnv("enum Shape {Empty = 0, Circle = 1 {f64}, Labeled = 2 {string, u8}}")
    );
    assert_eq!(
        hash("Source.lookup"),
        fnv("callback_method #5 lookup(string) -> i64 throws LookupError")
    );
    assert_eq!(
        hash("Counter.with_step"),
        fnv("constructor with_step(i64, i64) -> Counter throws CounterError")
    );
    assert_eq!(
        hash("Counter.fan_out"),
        fnv("method fan_out(i32) -> iter<Counter>")
    );
    assert_eq!(
        hash("QuotaError.Exceeded"),
        fnv("code Exceeded = 3001 {i64, i64}")
    );
    assert_eq!(hash("Counter"), fnv("interface Counter"));
    assert_eq!(
        hash("Counter.start_later"),
        fnv("static start_later(i64) -> Counter async")
    );
}
