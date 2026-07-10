//! Procedural macros that turn safe, annotated Rust into the WeaveFFI C ABI.
//!
//! A producer annotates an ordinary Rust module with `#[weaveffi::module]` and
//! tags the items it wants to export. The macro lowers the module to the
//! WeaveFFI IR (through [`weaveffi_bridge`]), builds the canonical
//! [`BindingModel`](weaveffi_core::model::BindingModel), and emits the
//! `#[no_mangle] extern "C"` thunks every generated language binding calls.
//! All of the `unsafe` marshalling lives in the `weaveffi-abi` runtime, so the
//! producer writes only safe Rust.
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
//! The same IR the macro lowers is what `weaveffi generate path/to/lib.rs`
//! reads, so the generated bindings and the producer cannot drift.
//!
//! # Attributes
//!
//! * [`macro@module`] marks an exported namespace (the driver attribute).
//! * [`macro@export`] exports a function; [`macro@record`] a by-value struct;
//!   [`macro@enumeration`] a `#[repr(i32)]` C-style enum.
//! * [`macro@interface`] declares an opaque object type whose `impl` block's
//!   `pub fn`s become constructors, methods, and statics.
//! * [`macro@error`] declares the module's error domain from a unit-variant
//!   enum with explicit discriminants.
//! * [`macro@callback`] / [`macro@listener`] declare a callback and an event
//!   listener; [`macro@cancellable`] marks an async function as cancellable.
//!
//! The item-level attributes are inert markers that [`macro@module`] reads; on
//! their own they expand to the item unchanged.

#![deny(missing_docs)]

use proc_macro::TokenStream;

mod codegen;

/// Mark an inline `mod` as an exported WeaveFFI namespace.
///
/// The macro re-emits the module unchanged and appends the generated C ABI
/// thunks for every tagged item it contains (functions, records, enums). Apply
/// it to a `mod foo { ... }` whose items carry the item-level markers.
#[proc_macro_attribute]
pub fn module(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_mod = syn::parse_macro_input!(item as syn::ItemMod);
    codegen::expand_module(&item_mod)
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
    /// Declare a by-value record (struct) with generated create/getters.
    record
}
marker_attr! {
    /// Declare an interface: an opaque object type with constructors, methods,
    /// and statics read from its `impl` block. Methods must take `&self`.
    interface
}
marker_attr! {
    /// Declare the module's error domain from a unit-variant enum with
    /// explicit discriminants. The module macro generates the matching
    /// `ErrorReport` implementation.
    error
}
marker_attr! {
    /// Declare a C-style `#[repr(i32)]` enum exported by value.
    enumeration
}
marker_attr! {
    /// Declare a callback function signature the host implements.
    callback
}
marker_attr! {
    /// Declare an event listener; takes `event = "CallbackName"`.
    listener
}
marker_attr! {
    /// Mark an async function as accepting a cancellation token.
    cancellable
}
marker_attr! {
    /// Opt a record into a generated fluent builder.
    builder
}
