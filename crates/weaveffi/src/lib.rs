//! WeaveFFI: write safe Rust, get a stable C ABI and bindings for 11 languages.
//!
//! This is the single crate a Rust producer depends on. Annotate an ordinary
//! module with [`macro@module`], tag the items you want to export, and call
//! [`export_runtime!`] once. The [`macro@module`] expansion emits the
//! `extern "C"` thunks that the generated language bindings call, marshalling
//! every argument and result through the audited [`abi`] runtime so you never
//! write `unsafe` glue by hand. Every C symbol starts with the crate's name
//! (`calculator_math_add` below).
//!
//! ```ignore
//! #[weaveffi::module]
//! pub mod math {
//!     /// The module's error domain; `Display` supplies the runtime message.
//!     #[weaveffi::error]
//!     #[derive(Debug)]
//!     pub enum MathError {
//!         /// Division by zero.
//!         DivisionByZero = 1,
//!     }
//!
//!     impl std::fmt::Display for MathError {
//!         fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//!             f.write_str("division by zero")
//!         }
//!     }
//!
//!     /// Add two integers.
//!     #[weaveffi::export]
//!     pub fn add(a: i32, b: i32) -> i32 {
//!         a + b
//!     }
//!
//!     /// Divide, reporting division by zero through the ABI's error channel.
//!     #[weaveffi::export]
//!     pub fn div(a: i32, b: i32) -> Result<i32, MathError> {
//!         a.checked_div(b).ok_or(MathError::DivisionByZero)
//!     }
//! }
//!
//! // Export the runtime symbols (memory, errors, cancel tokens) once.
//! weaveffi::export_runtime!();
//! ```
//!
//! The macro also embeds the API in the built library, which is what
//! `weaveffi generate` (run in the crate) reads to emit the header and
//! bindings, so the producer and the bindings can't drift: the bindings
//! describe exactly what the library was compiled with.
//!
//! # What you get
//!
//! * [`macro@module`] - the driver attribute on an exported `mod`.
//! * [`macro@export`] - export a function (`async fn` is asynchronous; a
//!   `Result`-returning fn is fallible).
//! * [`macro@record`] - a by-value struct serialized across the ABI.
//! * [`macro@enumeration`] - a `#[repr(i32)]` C-style enum, or a rich enum
//!   with data-carrying variants.
//! * [`macro@interface`] - an opaque, reference-counted object type; pass one
//!   as `&T` or `Arc<T>`, return one as `Self`, `T`, or `Arc<T>`.
//! * [`macro@error`] - the module's error domain.
//! * [`macro@callback_interface`] - a trait the consumer implements; accept
//!   one as `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`). Its methods
//!   return `Result<T, ForeignError>`; [`macro@throws`] lets one report the
//!   module's domain errors.
//! * [`macro@cancellable`] - mark an `async fn` as accepting a cancel token.
//! * [`set_spawner`] - install the executor async exports run on; the default
//!   is a small pool of worker threads, or Tokio with the `tokio` feature.
//! * [`export_runtime!`] - export the runtime symbols (memory, errors, cancel
//!   tokens, ABI version) under the crate's prefix, once per library.
//! * [`abi`] - the C ABI runtime: the error struct, memory helpers, and the
//!   marshalling converters the expansion calls.
//!
//! # Features
//!
//! * `leak-check` counts live objects, callbacks, iterators, cancel tokens,
//!   and returned allocations, reported by `{prefix}_debug_live` so a test
//!   harness can assert a consumer released everything. Off by default.
//! * `tokio` runs exported `async fn`s on Tokio: the current runtime when a
//!   launcher is called from inside one, otherwise a multi-thread runtime
//!   created on first use. [`set_spawner`] still overrides it. Off by
//!   default.

#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::missing_safety_doc)]
#![warn(clippy::doc_markdown)]

pub mod abi;

/// An owned, lazily-pulled iterator returned by a producer function whose IDL
/// return type is `iter<T>`. Construct one from any iterator with
/// [`Iter::new`](abi::Iter::new); the [`macro@module`] expansion turns
/// it into the opaque iterator handle the generated bindings consume.
pub use abi::Iter;

/// The producer's handle on a consumer's cancel token, accepted as the final
/// parameter of a `#[weaveffi::cancellable]` `async fn`. When the consumer
/// cancels, the runtime drops the function's future and completes the call
/// with the cancelled code; poll
/// [`is_cancelled`](abi::CancelToken::is_cancelled) only for
/// cooperative cleanup (work on other threads, say).
pub use abi::CancelToken;

/// A consumer's callback-interface implementation failed. Every callback
/// trait method returns `Result<T, ForeignError>`, so the failure arrives as
/// an `Err`; a method marked `#[weaveffi::throws]` can decode a declared
/// domain error from it with [`ForeignError::domain`](abi::ForeignError::domain).
pub use abi::ForeignError;

/// A module's error domain: the trait the `#[weaveffi::error]` expansion
/// implements so [`ForeignError::domain`](abi::ForeignError::domain) can
/// decode a typed error a callback reported.
pub use abi::ErrorDomain;

/// Maps a producer error onto the ABI's `(code, message)` pair. A fallible
/// `#[weaveffi::export]` function reports `Err(e)` through its trailing
/// `out_err` slot using this trait: `String` and `&str` errors get the
/// generic code `-1` out of the box, while a `#[weaveffi::error]` enum (or a
/// manual [`ErrorReport`] impl) surfaces the named codes of an IDL error
/// domain, with its `Display` output as the message.
pub use abi::ErrorReport;

/// Install the process-wide executor that exported `async fn`s run on. Call it
/// once at startup (before the first async export is launched) to hand futures
/// to a runtime of your choice; until then, and if never called, futures run
/// on the default executor (a small worker pool, or Tokio with the `tokio`
/// feature).
pub use abi::set_spawner;

/// The executor hook [`set_spawner`] accepts: anything callable as
/// `Fn(BoxFuture)` that is `Send + Sync + 'static`.
pub use abi::Spawner;

/// The type-erased `Send + 'static` future a [`Spawner`] receives.
pub use abi::BoxFuture;

pub use weaveffi_macros::{
    callback_interface, cancellable, enumeration, error, export, export_runtime, interface, module,
    record, throws,
};
