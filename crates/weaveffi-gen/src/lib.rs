//! Code generation for WeaveFFI: the `LanguageBackend` trait, the
//! code-generation orchestrator, the C ABI header renderer, and the eleven
//! language generators under [`targets`].
//!
//! Every generator renders from the resolved API and binding model in
//! [`weaveffi_model`], so symbol names and parameter lowering are computed
//! once and shared.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

pub mod backend;
pub mod cabi;
pub mod cache;
pub mod capabilities;
pub mod codegen;
pub mod lang;
pub mod manifest;
pub mod package;
pub mod platform;
pub mod targets;
pub mod utils;
