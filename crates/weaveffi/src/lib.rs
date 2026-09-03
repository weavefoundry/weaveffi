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
//! * [`macro@record`] - a by-value struct serialized across the ABI.
//! * [`macro@enumeration`] - a `#[repr(i32)]` C-style enum, or a rich enum
//!   with data-carrying variants.
//! * [`macro@interface`] - an opaque, reference-counted object type; pass one
//!   as `&T` or `Arc<T>`, return one as `Self`, `T`, or `Arc<T>`.
//! * [`macro@error`] - the module's error domain.
//! * [`macro@callback_interface`] - a trait the consumer implements; accept
//!   one as `Arc<dyn Trait>`.
//! * [`macro@cancellable`] - mark an `async fn` as accepting a cancel token.
//! * [`set_spawner`] - install the executor async exports run on (Tokio, for
//!   example); the default drives each future on its own thread.
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
/// `out_err` slot using this trait: `String` and `&str` errors get the
/// generic code `-1` out of the box, while a `#[weaveffi::error]` enum (or a
/// manual [`ErrorReport`] impl) surfaces the named codes of an IDL error
/// domain.
pub use weaveffi_abi::ErrorReport;

/// Install the process-wide executor that exported `async fn`s run on. Call it
/// once at startup (before the first async export is launched) to hand futures
/// to a runtime such as Tokio; until then, and if never called, each future is
/// driven to completion on its own thread.
pub use weaveffi_abi::set_spawner;

/// The executor hook [`set_spawner`] accepts: anything callable as
/// `Fn(BoxFuture)` that is `Send + Sync + 'static`.
pub use weaveffi_abi::Spawner;

/// The type-erased `Send + 'static` future a [`Spawner`] receives.
pub use weaveffi_abi::BoxFuture;

pub use weaveffi_macros::{
    callback_interface, cancellable, enumeration, error, export, interface, module, record,
};
