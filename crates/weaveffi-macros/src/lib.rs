//! Procedural macros that turn safe, annotated Rust into the WeaveFFI C ABI.
//!
//! A producer annotates an ordinary Rust module with `#[weaveffi::module]`,
//! tags the items it wants to export, and calls `weaveffi::export_runtime!()`
//! once. The module macro lowers the module tree to the WeaveFFI IR, builds
//! the canonical [`Model`](weaveffi_model::model::Model) with the shared
//! validator, and emits the `extern "C"` thunks every generated language
//! binding calls. All of the `unsafe` marshalling lives in the
//! `weaveffi::abi` runtime, so the producer writes only safe Rust.
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
//! Every C symbol starts with the crate's name (`CARGO_CRATE_NAME`). The
//! macro also embeds the module's API in the library as metadata, which
//! `weaveffi generate` reads back out of the built library, so the generated
//! bindings describe exactly what the library was compiled with.
//!
//! # Attributes
//!
//! * [`macro@module`] marks an exported namespace (the driver attribute).
//! * [`macro@export`] exports a function; [`macro@record`] a by-value struct;
//!   [`macro@enumeration`] a `#[repr(i32)]` C-style enum or a rich enum with
//!   data-carrying variants.
//! * [`macro@interface`] declares an opaque, reference-counted object type
//!   whose `impl` blocks' `pub fn`s become constructors, methods, and
//!   statics; [`macro@skip`] leaves one of them unexported.
//! * [`macro@error`] declares an error domain from an enum with explicit
//!   discriminants; a variant's named fields become the code's structured
//!   payload, and the macro generates `Display` and `std::error::Error`.
//! * [`macro@callback_interface`] declares a trait the consumer implements,
//!   whose methods return `Result<T, E>` with `E: From<ForeignError>`.
//! * [`macro@custom`] declares a type alias that crosses as another type.
//! * [`macro@cancellable`] marks an async function as cancellable.
//! * [`export_runtime!`] emits the runtime symbols (memory, errors, cancel
//!   tokens, ABI version) once per library.
//!
//! The item-level attributes are markers that [`macro@module`] reads and
//! removes; one that's left to expand on its own (because it isn't inside a
//! `#[weaveffi::module]`) is a compile error.

#![deny(missing_docs)]

use proc_macro::TokenStream;

mod codegen;
mod extract;
mod runtime;

/// Mark an inline `mod` as an exported WeaveFFI namespace.
///
/// The macro validates the module tree with the same rules the CLI applies
/// to an IDL, re-emits the module, and appends the generated C ABI thunks
/// for every tagged item it contains, recursing into nested
/// `#[weaveffi::module]` submodules (whose symbols carry the joined module
/// path). A `#[cfg]` on an exported item applies to its thunks too. A
/// top-level module also exports `{prefix}_{module}_contract()`, the
/// contract table generated bindings verify when they load the library.
#[proc_macro_attribute]
pub fn module(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_mod = syn::parse_macro_input!(item as syn::ItemMod);
    codegen::expand_module(&item_mod)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Emit the runtime symbols every WeaveFFI library exports, prefixed with the
/// crate's name: `{prefix}_abi_version`, `{prefix}_error_set`,
/// `{prefix}_error_set_payload`, `{prefix}_error_clear`,
/// `{prefix}_error_free`, `{prefix}_alloc`, `{prefix}_free_bytes`, the four
/// `{prefix}_cancel_token_*` functions, and `{prefix}_debug_live`.
///
/// Invoke it exactly once, at the crate root of the `cdylib`. It takes no
/// arguments.
#[proc_macro]
pub fn export_runtime(input: TokenStream) -> TokenStream {
    runtime::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generate the marker attributes that [`macro@module`] reads and strips.
/// Expanding on their own means they aren't inside a `#[weaveffi::module]`,
/// so each reports that and passes the item through unchanged (so the item's
/// other errors still surface).
macro_rules! marker_attr {
    ($(#[$meta:meta])* $name:ident, $placement:literal) => {
        $(#[$meta])*
        #[proc_macro_attribute]
        pub fn $name(_attr: TokenStream, item: TokenStream) -> TokenStream {
            misplaced(stringify!($name), $placement, item)
        }
    };
}

/// A `compile_error!` for a marker that expanded on its own, pointing at the
/// attribute, followed by the item.
fn misplaced(name: &str, placement: &str, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);
    let span = proc_macro2::Span::call_site();
    let message = format!(
        "`#[weaveffi::{name}]` only works {placement}; put the item in an inline module \
         annotated `#[weaveffi::module]` (`#[weaveffi::module] pub mod api {{ ... }}`)"
    );
    let error = quote::quote_spanned!(span=> ::core::compile_error!(#message););
    quote::quote!(#error #item).into()
}

marker_attr! {
    /// Export a function across the FFI boundary. An `async fn` lowers to an
    /// asynchronous symbol. A `fn -> Result<T, E>` is fallible: it throws
    /// `E` when `E` is a `#[weaveffi::error]` enum of the module tree, and
    /// otherwise reports `E`'s `Display` output as an untyped error
    /// (`throws any`).
    export, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare a by-value record (struct) serialized in the value-buffer
    /// format when it crosses the ABI.
    record, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare an interface: an opaque, reference-counted object type with
    /// constructors, methods, and statics read from its `impl` blocks.
    /// Methods take `&self` or `self: Arc<Self>`; the type must be
    /// `Send + Sync`.
    interface, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare an error domain from an enum with explicit discriminants (the
    /// stable error codes). The macro generates `Display` (each variant's
    /// `#[weaveffi(message = "...")]` template, which may name the
    /// variant's fields in braces, else the first line of its doc comment)
    /// and `std::error::Error` (so the enum must derive `Debug`), unless
    /// written `#[weaveffi::error(no_display)]`. A module may declare
    /// several.
    error, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare an enum exported by value: a `#[repr(i32)]` C-style enum, or a
    /// rich enum whose variants carry named fields.
    enumeration, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare a callback interface: a trait whose `&self` methods the
    /// consumer implements. Producers accept one as `Arc<dyn Trait>` (or
    /// `Option<Arc<dyn Trait>>`). Every method returns `Result<T, E>` with
    /// `E: From<weaveffi::ForeignError>`; when `E` is a `#[weaveffi::error]`
    /// domain of the module tree, the method throws that domain and the
    /// consumer's typed errors arrive as its variants.
    callback_interface, "on an item inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Mark an `async fn` as cancellable: it takes a `weaveffi::CancelToken`
    /// as its final parameter, and cancelling the token completes the call
    /// with the cancelled code.
    cancellable, "on an exported `async fn` inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Declare a custom type on a type alias,
    /// `#[weaveffi::custom(repr = R, lift = f, lower = g)] pub type Name = T;`:
    /// `Name` crosses the ABI as `R` (bindings see `R`), converted with
    /// `f: fn(R) -> Result<T, E: Display>` on the way in (a failure is a
    /// marshalling error carrying the message) and `g: fn(&T) -> R` on the
    /// way out.
    custom, "on a type alias inside a `#[weaveffi::module]`"
}
marker_attr! {
    /// Leave a `pub fn` of an interface's `impl` block out of the exported
    /// interface.
    skip, "on a `pub fn` in the `impl` block of a `#[weaveffi::interface]` inside a `#[weaveffi::module]`"
}
