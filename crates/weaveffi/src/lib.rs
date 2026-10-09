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
//!     /// The module's error domain. Its `Display` comes from the messages.
//!     #[weaveffi::error]
//!     #[derive(Debug)]
//!     pub enum MathError {
//!         /// Division by zero.
//!         DivisionByZero = 1,
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
//!
//!     /// Parse an integer; any error type with `Display` works (`throws any`).
//!     #[weaveffi::export]
//!     pub fn parse(text: &str) -> Result<i64, std::num::ParseIntError> {
//!         text.parse()
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
//!   `Result`-returning fn is fallible: `throws` its error type when that's
//!   a [`macro@error`] enum of the module tree, else `throws any`).
//! * [`macro@record`] - a by-value struct serialized across the ABI.
//! * [`macro@enumeration`] - a `#[repr(i32)]` C-style enum, or a rich enum
//!   with data-carrying variants.
//! * [`macro@interface`] - an opaque, reference-counted object type; pass one
//!   as `&T` or `Arc<T>`, return one as `Self`, `T`, or `Arc<T>`.
//!   [`macro@skip`] leaves a `pub fn` of its `impl` unexported.
//! * [`macro@error`] - an error domain; the macro generates its `Display`
//!   (from `#[weaveffi(message = "...")]` templates or the variants' docs)
//!   and `std::error::Error`.
//! * [`macro@callback_interface`] - a trait the consumer implements; accept
//!   one as `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`). Its methods
//!   return `Result<T, E>` with `E: From<ForeignError>`; when `E` is a
//!   declared domain, the consumer's typed errors arrive typed.
//! * [`macro@custom`] - a type alias that crosses as another type (its
//!   repr), converted with your `lift` and `lower` functions.
//! * [`macro@cancellable`] - mark an `async fn` as accepting a cancel token.
//! * [`set_spawner`] - install the executor async exports run on; the default
//!   is Tokio (the `tokio` feature), else a thread per call.
//! * [`export_runtime!`] - export the runtime symbols (memory, errors, cancel
//!   tokens, ABI version) under the crate's prefix, once per library. A
//!   crate whose modules use the macros fails to compile without it.
//! * [`abi`] - the C ABI runtime: the error struct, memory helpers, and the
//!   marshalling converters the expansion calls.
//!
//! # Features
//!
//! * `tokio` (default) runs exported `async fn`s on Tokio: the current
//!   runtime when a launcher is called from inside one, otherwise a
//!   multi-thread runtime created on first use. Without it, each async call
//!   runs on a thread of its own. [`set_spawner`] overrides either. On
//!   `wasm32` the feature has no effect: calls are polled inline.
//! * `leak-check` counts live objects, callbacks, iterators, cancel tokens,
//!   and returned allocations, reported by `{prefix}_debug_live` so a test
//!   harness can assert a consumer released everything. Off by default.

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

/// A consumer's callback-interface implementation failed. A callback trait
/// method's error type converts from it (`E: From<ForeignError>`), and it's
/// a valid error type itself; [`ForeignError::domain`](abi::ForeignError::domain)
/// decodes a declared domain error from one.
pub use abi::ForeignError;

/// A declared error domain: the trait the `#[weaveffi::error]` expansion
/// implements, mapping each variant to its code, message, and payload.
pub use abi::ErrorDomain;

/// Install the process-wide executor that exported `async fn`s run on. Call it
/// once at startup (before the first async export is launched) to hand futures
/// to a runtime of your choice; until then, and if never called, futures run
/// on the default executor (Tokio with the default `tokio` feature, else a
/// thread per call).
pub use abi::set_spawner;

/// The executor hook [`set_spawner`] accepts: anything callable as
/// `Fn(BoxFuture)` that is `Send + Sync + 'static`.
pub use abi::Spawner;

/// The type-erased `Send + 'static` future a [`Spawner`] receives.
pub use abi::BoxFuture;

pub use weaveffi_macros::{
    callback_interface, cancellable, custom, enumeration, error, export, export_runtime, interface,
    module, record, skip,
};
