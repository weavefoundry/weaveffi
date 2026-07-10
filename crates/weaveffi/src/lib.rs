//! WeaveFFI: write safe Rust, get a stable C ABI and bindings for 11 languages.
//!
//! This is the single crate a Rust producer depends on. Annotate an ordinary
//! module with [`macro@module`], tag the items you want to export, and call
//! [`export_runtime!`] once. The [`macro@module`] expansion emits the
//! `#[no_mangle] extern "C"` thunks that the generated language bindings call,
//! marshalling every argument and result through the audited [`abi`] runtime so
//! you never write `unsafe` glue by hand.
//!
//! ```ignore
//! #[weaveffi::module]
//! pub mod calculator {
//!     /// Add two integers.
//!     #[weaveffi::export]
//!     pub fn add(a: i32, b: i32) -> i32 {
//!         a + b
//!     }
//!
//!     /// Divide, reporting division by zero through the ABI's error channel.
//!     #[weaveffi::export]
//!     pub fn div(a: i32, b: i32) -> Result<i32, String> {
//!         if b == 0 {
//!             return Err("division by zero".to_string());
//!         }
//!         Ok(a / b)
//!     }
//! }
//!
//! // Expose the fixed runtime surface (memory/error/cancel helpers) once.
//! weaveffi::export_runtime!();
//! ```
//!
//! The same annotated source is what `weaveffi generate path/to/lib.rs` reads to
//! emit the IDL, header, and bindings, so the producer and the bindings cannot
//! drift: they are two views of one parse.
//!
//! # What you get
//!
//! * [`macro@module`] - the driver attribute on an exported `mod`.
//! * [`macro@export`] - export a function (`async fn` is asynchronous; a
//!   `Result`-returning fn is fallible).
//! * [`macro@record`] - a by-value struct with generated create/getters.
//! * [`macro@enumeration`] - a `#[repr(i32)]` C-style enum.
//! * [`macro@callback`] / [`macro@listener`] - a callback and an event listener.
//! * [`macro@cancellable`] - mark an `async fn` as accepting a cancel token;
//!   [`macro@builder`] - opt a record into a fluent builder.
//! * [`abi`] - the C ABI runtime: the error struct, memory helpers, the
//!   marshalling converters the expansion calls, and [`export_runtime!`].

#![deny(missing_docs)]

/// The stable C ABI runtime: error type, cancel tokens, memory management, and
/// the `lift_*`/`lower_*` marshalling converters the macro expansion calls.
///
/// Re-exported from [`weaveffi_abi`] so producers depend on a single `weaveffi`
/// crate; the generated thunks reference these items as `::weaveffi::abi::*`.
pub use weaveffi_abi as abi;

pub use weaveffi_abi::export_runtime;

/// An owned, lazily-pulled iterator returned by a producer function whose IDL
/// return type is `iter<T>`. Construct one from any iterator with
/// [`Iter::new`](weaveffi_abi::Iter::new); the [`macro@module`] expansion turns
/// it into the opaque iterator handle the generated bindings consume.
pub use weaveffi_abi::Iter;

/// A `Send` view of a foreign cancellation token, accepted as the final
/// parameter of a `#[weaveffi::cancellable]` `async fn`. Poll
/// [`is_cancelled`](weaveffi_abi::CancelToken::is_cancelled) at safe points and
/// return early when it reports cancellation; the [`macro@module`] expansion
/// supplies the token from the async launcher's `cancel_token` slot.
pub use weaveffi_abi::CancelToken;

/// Maps a producer error onto the ABI's `(code, message)` pair. A fallible
/// `#[weaveffi::export]` function reports `Err(e)` through its trailing
/// `out_err` slot using this trait, so every [`std::fmt::Display`] error gets
/// the generic code `-1`, while a type that implements
/// [`ErrorReport`] directly surfaces the named codes
/// of an IDL error domain.
pub use weaveffi_abi::ErrorReport;

pub use weaveffi_macros::{
    builder, callback, cancellable, enumeration, error, export, interface, listener, module, record,
};
