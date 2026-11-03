//! Procedural macros that turn safe, annotated Rust into the WeaveFFI C ABI.
//!
//! A producer tags the items it exports, anywhere in the crate, and calls
//! `weaveffi::export_runtime!()` once at the crate root:
//!
//! ```ignore
//! /// Add two integers.
//! #[weaveffi::export]
//! pub fn add(a: i32, b: i32) -> i32 {
//!     a + b
//! }
//!
//! weaveffi::export_runtime!();
//! ```
//!
//! Each attribute expands on its own. It emits the item's `extern "C"`
//! functions, typed through the runtime's value traits (so rustc resolves
//! every type wherever it's declared), and embeds the item's metadata frame
//! and contract function in the library. `weaveffi generate` reads the
//! frames back out of the built library, so the generated bindings describe
//! exactly what the library was compiled with. Every C symbol starts with
//! the crate's name (`CARGO_CRATE_NAME`), and a library is one namespace.
//!
//! # Attributes
//!
//! * [`macro@export`] on a function, or on an interface's `impl` block
//!   (whose `pub fn`s become constructors, methods, and statics; a
//!   `#[weaveffi(skip)]` member is left out).
//! * [`macro@record`] on a struct with named fields.
//! * [`macro@enumeration`] on a C-style enum or one with named-field
//!   variants.
//! * [`macro@error`] on an error-domain enum with explicit positive codes.
//! * [`macro@interface`] on a reference-counted object type.
//! * [`macro@callback_interface`] on a trait the consumer implements.
//! * [`macro@custom`] on a type alias or struct that crosses as another type.
//! * [`export_runtime!`] for the runtime symbols and the crate's tag.

#![deny(missing_docs)]

use proc_macro::TokenStream;

mod callback;
mod common;
mod function;
mod runtime;
mod shape;
mod types;

fn emit(
    result: syn::Result<proc_macro2::TokenStream>,
    item: proc_macro2::TokenStream,
) -> TokenStream {
    match result {
        Ok(tokens) => tokens.into(),
        // Keep the item, so its own errors and its uses still type-check
        // (and an IDE still sees it), next to the macro's error.
        Err(e) => {
            let error = e.into_compile_error();
            quote::quote!(#error #item).into()
        }
    }
}

/// Export a function, or every `pub fn` of an interface's `impl` block.
///
/// A function's parameters may be values (`T`), borrows (`&str`, `&[u8]`,
/// `&Store`, `&Entry`, `Option<&T>`), or callback interfaces
/// (`Arc<dyn Trait>`, `Option<Arc<dyn Trait>>`). It returns nothing, a
/// value, a `weaveffi::Iter<T>` (lazily pulled), or a `Result` of one,
/// whose error is a `#[weaveffi::error]` domain or an untyped error
/// (`weaveffi::Error`, `String`, `std::io::Error`, ...). An `async fn` is
/// asynchronous, and cancellable when its last parameter is a
/// `weaveffi::CancelToken`.
///
/// In an `impl` block, a member with `&self` (or `self: Arc<Self>`) is a
/// method, a synchronous one returning the type (`Self`, `Arc<Self>`, or a
/// `Result` of either) a constructor, and any other a static (so an async
/// factory is a static that returns the interface).
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::Item);
    emit(function::expand(attr.into(), parsed), copy)
}

/// Declare a record: a struct with named fields that crosses the ABI by
/// value (serialized in a value buffer).
#[proc_macro_attribute]
pub fn record(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::ItemStruct);
    emit(types::record(attr.into(), parsed), copy)
}

/// Declare an enum: C-style (every variant a unit, crossing as an `int32_t`)
/// or rich (variants with named fields, crossing as a value buffer). A
/// variant's discriminant is its `= value`, or one more than the previous
/// variant's.
#[proc_macro_attribute]
pub fn enumeration(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::ItemEnum);
    emit(types::enumeration(attr.into(), parsed), copy)
}

/// Declare an error domain from an enum whose variants have explicit
/// positive values (the stable codes) and optional named fields (the
/// code's payload). The macro generates `Display` (each variant's
/// `#[weaveffi(message = "...")]` template, which may name its fields in
/// braces, else the first line of its docs) and `std::error::Error`, unless
/// written `#[weaveffi::error(no_display)]`.
#[proc_macro_attribute]
pub fn error(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::ItemEnum);
    emit(types::error(attr.into(), parsed), copy)
}

/// Declare an interface: a reference-counted object type, which must be
/// `Send + Sync`. Export its `impl` block with `#[weaveffi::export]`.
#[proc_macro_attribute]
pub fn interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::ItemStruct);
    emit(types::interface(attr.into(), parsed), copy)
}

/// Declare a callback interface: a `Send + Sync` trait whose `&self`
/// methods the consumer implements. Producers accept one as
/// `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`). Every method returns
/// `Result<T, E>` with `E: From<weaveffi::ForeignError>`; when `E` is a
/// `#[weaveffi::error]` domain, the consumer's typed errors arrive as its
/// variants.
#[proc_macro_attribute]
pub fn callback_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::ItemTrait);
    emit(callback::expand(attr.into(), parsed), copy)
}

/// Declare a custom type,
/// `#[weaveffi::custom(repr = R, lift = f, lower = g)] pub type Name = T;`
/// (or on a struct): `T` crosses the ABI as `R` (bindings see `R`),
/// converted with `f: fn(R) -> Result<T, E: Display>` on the way in (a
/// failure is a marshalling error carrying the message) and
/// `g: fn(&T) -> R` on the way out. `T` may be a foreign type, such as
/// `uuid::Uuid`.
#[proc_macro_attribute]
pub fn custom(attr: TokenStream, item: TokenStream) -> TokenStream {
    let copy = proc_macro2::TokenStream::from(item.clone());
    let parsed = syn::parse_macro_input!(item as syn::Item);
    emit(types::custom(attr.into(), parsed), copy)
}

/// Emit the runtime symbols every WeaveFFI library exports, prefixed with
/// the crate's name (`{prefix}_abi_version`, `{prefix}_error_set`,
/// `{prefix}_error_set_payload`, `{prefix}_error_clear`,
/// `{prefix}_error_free`, `{prefix}_alloc`, `{prefix}_run_free`, the four
/// `{prefix}_cancel_token_*` functions, and `{prefix}_debug_live`), and the
/// crate's tag type. Invoke it exactly once, at the crate root.
#[proc_macro]
pub fn export_runtime(input: TokenStream) -> TokenStream {
    runtime::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
