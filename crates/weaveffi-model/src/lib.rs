//! The WeaveFFI API model: everything between an API definition and code
//! generation.
//!
//! * [`ir`] holds the in-memory IR types that an IDL document or a Rust
//!   producer's metadata becomes, and [`parse`] reads the IDL text formats
//!   (YAML and JSON), resolving imports.
//! * [`validate`] checks a document against the library's
//!   [`Identity`](pkg::Identity) and builds the one [`Model`](model::Model)
//!   every generator consumes.
//! * [`ty`], [`model`], [`abi`], [`errors`], and [`pkg`] hold the resolved
//!   types and type index, the model with every C symbol and ABI signature,
//!   the C type algebra, the error naming policy, and the package identity.
//! * [`contract`] computes the contract, one fingerprint per declaration, so
//!   a stale binding refuses to load and names what changed.
//! * [`meta`] is the library metadata: the frames the producer macros embed
//!   in a library and the CLI reads back into an [`Api`](ir::Api).
//!
//! # Features
//!
//! * `idl` (default): the IDL text formats, JSON Schema derivation for the IR
//!   types, and fancy miette diagnostics. Without it the crate still provides
//!   the IR, validation, the model, the contract signature builder, and the
//!   metadata frames, which is all the producer macros need.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]
// A few docs link to IDL-only items (the `parse` module, miette's
// `Diagnostic`); those links only resolve with the `idl` feature.
#![cfg_attr(not(feature = "idl"), allow(rustdoc::broken_intra_doc_links))]

pub mod abi;
pub mod contract;
pub mod errors;
pub mod ir;
pub mod meta;
pub mod model;
#[cfg(feature = "idl")]
pub mod parse;
pub mod pkg;
pub mod ty;
pub mod validate;
