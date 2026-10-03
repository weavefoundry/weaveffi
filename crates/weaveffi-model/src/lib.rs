//! The WeaveFFI API model: everything between an API definition and code
//! generation.
//!
//! * [`ir`] holds the in-memory IR types that an IDL document or annotated
//!   Rust source lowers to, and [`parse`] reads the IDL text formats (YAML,
//!   JSON, and TOML).
//! * [`rust`] extracts the IR from annotated Rust source; the proc-macros and
//!   the CLI share it.
//! * [`validate`] checks a document and produces the [`ResolvedApi`] every
//!   consumer works from.
//! * [`model`], [`abi`], [`plan`], [`errors`], and [`pkg`] build the canonical
//!   binding model, the C ABI lowering, the marshalling plan, the error
//!   mapping, and the package identity.
//! * [`checksum`] fingerprints each top-level module's contract so a stale
//!   binding refuses to load.
//!
//! # Features
//!
//! * `idl` (default): the IDL text formats, JSON Schema derivation for the IR
//!   types, and fancy miette diagnostics. Without it the crate still provides
//!   the IR types, Rust extraction, validation, and the binding model, which
//!   is all the proc-macros need.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]
// A few docs link to IDL-only items (the `parse` module, miette's
// `Diagnostic`); those links only resolve with the `idl` feature.
#![cfg_attr(not(feature = "idl"), allow(rustdoc::broken_intra_doc_links))]

pub mod abi;
pub mod checksum;
pub mod errors;
pub mod ir;
pub mod model;
#[cfg(feature = "idl")]
pub mod parse;
pub mod pkg;
pub mod plan;
pub mod resolved;
pub mod rust;
pub mod validate;

pub use resolved::ResolvedApi;
