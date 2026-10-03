//! Procedural macros that turn safe, annotated Rust into the WeaveFFI C ABI.
//!
//! A producer annotates an ordinary Rust module with `#[weaveffi::module]`,
//! tags the items it wants to export, and calls `weaveffi::export_runtime!()`
//! once. The module macro lowers the module tree to the WeaveFFI IR (through
//! [`weaveffi_model::rust`]), builds the canonical
//! [`BindingModel`](weaveffi_model::model::BindingModel), and emits the
//! `extern "C"` thunks every generated language binding calls. All of the
//! `unsafe` marshalling lives in the `weaveffi-abi` runtime, so the producer
//! writes only safe Rust.
//!
//! ```ignore
//! #[weaveffi::module]
//! pub mod calculator {
//!     /// Add two integers.
//!     #[weaveffi::export]
//!     pub fn add(a: i32, b: i32) -> i32 {
//!         a + b
//!     }
//! }
//!
//! weaveffi::export_runtime!();
//! ```
//!
//! Every C symbol starts with the crate's name (`CARGO_CRATE_NAME`), which
//! is also the prefix `weaveffi generate` derives for a `.rs` input, so the
//! generated bindings and the producer can't drift: they're two views of one
//! parse.
//!
//! # Attributes
//!
//! * [`macro@module`] marks an exported namespace (the driver attribute).
//! * [`macro@export`] exports a function; [`macro@record`] a by-value struct;
//!   [`macro@enumeration`] a `#[repr(i32)]` C-style enum or a rich enum with
//!   data-carrying variants.
//! * [`macro@interface`] declares an opaque, reference-counted object type
//!   whose `impl` block's `pub fn`s become constructors, methods, and statics.
//! * [`macro@error`] declares the module's error domain from an enum with
//!   explicit discriminants; a variant's named fields become the code's
//!   structured payload.
//! * [`macro@callback_interface`] declares a trait the consumer implements;
//!   [`macro@cancellable`] marks an async function as cancellable.
//! * [`export_runtime!`] emits the runtime symbols (memory, errors, cancel
//!   tokens, ABI version) once per library.
//!
//! The item-level attributes are inert markers that [`macro@module`] reads; on
//! their own they expand to the item unchanged.

#![deny(missing_docs)]

use proc_macro::TokenStream;

mod codegen;
mod runtime;

/// Mark an inline `mod` as an exported WeaveFFI namespace.
///
/// The macro re-emits the module and appends the generated C ABI thunks for
/// every tagged item it contains, recursing into nested `#[weaveffi::module]`
/// submodules (whose symbols carry the joined module path). A top-level
/// module also exports `{prefix}_{module}_checksum()`, the contract checksum
/// generated bindings verify when they load the library.
#[proc_macro_attribute]
pub fn module(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_mod = syn::parse_macro_input!(item as syn::ItemMod);
    codegen::expand_module(&item_mod)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Emit the runtime symbols every WeaveFFI library exports, prefixed with the
/// crate's name: `{prefix}_abi_version`, `{prefix}_error_set`,
/// `{prefix}_error_clear`, `{prefix}_error_free`, `{prefix}_free_bytes`, the
/// four `{prefix}_cancel_token_*` functions, `{prefix}_debug_live`, and on
/// `wasm32` `{prefix}_alloc` and `{prefix}_dealloc`.
///
/// Invoke it exactly once, at the crate root of the `cdylib`. It takes no
/// arguments.
#[proc_macro]
pub fn export_runtime(input: TokenStream) -> TokenStream {
    runtime::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generate `#[doc(hidden)]` no-op marker attributes that [`macro@module`]
/// reads. Each expands to the annotated item unchanged.
macro_rules! marker_attr {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[proc_macro_attribute]
        pub fn $name(_attr: TokenStream, item: TokenStream) -> TokenStream {
            item
        }
    };
}

marker_attr! {
    /// Export a function across the FFI boundary. An `async fn` lowers to an
    /// asynchronous symbol; a `fn -> Result<T, E>` is fallible.
    export
}
marker_attr! {
    /// Declare a by-value record (struct) serialized in the value-buffer
    /// format when it crosses the ABI.
    record
}
marker_attr! {
    /// Declare an interface: an opaque, reference-counted object type with
    /// constructors, methods, and statics read from its `impl` block. Methods
    /// take `&self` or `self: Arc<Self>`; the type must be `Send + Sync`.
    interface
}
marker_attr! {
    /// Declare the module's error domain from an enum with explicit
    /// discriminants (the stable error codes). The enum must implement
    /// `std::fmt::Display`, which supplies the runtime message; each
    /// variant's doc comment is the documented default message.
    error
}
marker_attr! {
    /// Declare an enum exported by value: a `#[repr(i32)]` C-style enum, or a
    /// rich enum whose variants carry named fields.
    enumeration
}
marker_attr! {
    /// Declare a callback interface: a trait whose `&self` methods the
    /// consumer implements. Producers accept one as `Arc<dyn Trait>`. A
    /// method may return `Result<T, weaveffi::ForeignError>` to receive the
    /// consumer's failure as a value.
    callback_interface
}
marker_attr! {
    /// Mark an `async fn` as cancellable: it takes a `weaveffi::CancelToken`
    /// as its final parameter, and cancelling the token completes the call
    /// with the cancelled code.
    cancellable
}
