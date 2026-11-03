//! WeaveFFI: write safe Rust, get a stable C ABI and bindings for 11
//! languages.
//!
//! This is the one crate a Rust producer depends on. Tag the items you want
//! to export, anywhere in the crate, and call [`export_runtime!`] once at
//! the crate root. Every expansion emits the `extern "C"` functions the
//! generated bindings call, marshalling each value through the audited
//! runtime, so you never write `unsafe` glue. Every C symbol starts with the
//! crate's name (`calculator_add` below).
//!
//! ```ignore
//! /// The library's error domain. Its `Display` comes from the docs.
//! #[weaveffi::error]
//! #[derive(Debug)]
//! pub enum MathError {
//!     /// division by zero
//!     DivisionByZero = 1,
//! }
//!
//! /// Add two integers.
//! #[weaveffi::export]
//! pub fn add(a: i32, b: i32) -> i32 {
//!     a + b
//! }
//!
//! /// Divide, reporting division by zero through the ABI's error channel.
//! #[weaveffi::export]
//! pub fn div(a: i32, b: i32) -> Result<i32, MathError> {
//!     a.checked_div(b).ok_or(MathError::DivisionByZero)
//! }
//!
//! /// Parse an integer; any error converts into `weaveffi::Error`
//! /// (`throws any`).
//! #[weaveffi::export]
//! pub fn parse(text: &str) -> Result<i64, weaveffi::Error> {
//!     Ok(text.parse()?)
//! }
//!
//! // The runtime symbols (memory, errors, cancel tokens), once per crate.
//! weaveffi::export_runtime!();
//! ```
//!
//! The attributes also embed the API in the built library, which is what
//! `weaveffi generate` reads to emit the header and bindings, so the
//! producer and the bindings can't drift: the bindings describe exactly what
//! the library was compiled with. A library is one namespace: every
//! exported name is unique across the crate.
//!
//! # What you get
//!
//! * [`macro@export`]: export a free function, or every `pub fn` of an
//!   `impl` block of an interface (constructors return `Self`; methods take
//!   `&self`). An `async fn` is asynchronous; a `Result`-returning function
//!   throws its error type, a [`macro@error`] domain or an untyped error
//!   ([`Error`], `String`, `std::io::Error`, ...). An `async fn` whose last
//!   parameter is a [`CancelToken`] is cancellable.
//! * [`macro@record`]: a struct crossing by value.
//! * [`macro@enumeration`]: a C-style enum, or one with data-carrying
//!   variants.
//! * [`macro@interface`]: a reference-counted object type, passed as `&T`
//!   or `Arc<T>` and returned as `Self`, `T`, or `Arc<T>`.
//! * [`macro@error`]: an error domain, with a generated `Display` (from
//!   `#[weaveffi(message = "...")]` templates or the variants' docs) and
//!   `std::error::Error`.
//! * [`macro@callback_interface`]: a trait the consumer implements, accepted
//!   as `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`).
//! * [`macro@custom`]: a type that crosses as another (its repr), converted
//!   with your functions; it may be a foreign type such as `uuid::Uuid`.
//! * [`set_spawner`]: install the executor async exports run on.
//! * [`export_runtime!`]: the runtime symbols and the crate's tag type,
//!   once, at the crate root.
//!
//! # Features
//!
//! * `tokio` (default) runs exported `async fn`s on Tokio: the current
//!   runtime when a launcher is called from inside one, otherwise a
//!   multi-thread runtime created on first use. Without it, each async call
//!   runs on a thread of its own. [`set_spawner`] overrides either. On
//!   `wasm32` calls are polled inline.
//! * `leak-check` counts live objects, callbacks, iterators, cancel tokens,
//!   and runs, reported by `{prefix}_debug_live` so a test harness can
//!   assert a consumer released everything.
//! * `anyhow` lets a function return `Result<T, anyhow::Error>`.

#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::missing_safety_doc)]
#![warn(clippy::doc_markdown)]

#[doc(hidden)]
pub mod rt;

#[doc(hidden)]
pub use rt as __rt;

/// An owned, lazily pulled iterator, returned by a function whose bindings
/// see `iter<T>`. Build one from any iterator with [`Iter::new`].
pub use __rt::Iter;

/// The producer's handle on a consumer's cancel token, the final parameter
/// of a cancellable `async fn`. When the consumer cancels, the runtime drops
/// the function's future and completes the call with the cancelled code;
/// poll [`is_cancelled`](CancelToken::is_cancelled) only for cooperative
/// cleanup (work on other threads, say).
pub use __rt::CancelToken;

/// A consumer's callback-interface implementation failed. A callback
/// method's error type converts from it (`E: From<ForeignError>`), and it's
/// a valid error type itself.
pub use __rt::ForeignError;

/// An untyped error that any `std::error::Error` converts into with `?`.
pub use __rt::error::Error;

/// Install the process-wide executor exported `async fn`s run on. Call it
/// once at startup, before the first async export is launched.
pub use __rt::set_spawner;

/// The executor hook [`set_spawner`] accepts.
pub use __rt::Spawner;

/// The type-erased `Send + 'static` future a [`Spawner`] receives.
pub use __rt::BoxFuture;

pub use weaveffi_macros::{
    callback_interface, custom, enumeration, error, export, export_runtime, interface, record,
};
