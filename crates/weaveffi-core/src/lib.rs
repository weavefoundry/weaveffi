//! Core logic shared by every WeaveFFI generator and the CLI: validation, the
//! resolved API and binding model, the C ABI lowering, the marshalling plan,
//! the `LanguageBackend` trait, and code-generation orchestration.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

pub mod abi;
pub mod backend;
pub mod cabi;
pub mod cache;
pub mod capabilities;
pub mod codegen;
pub mod errors;
pub mod lang;
pub mod manifest;
pub mod model;
pub mod package;
pub mod pkg;
pub mod plan;
pub mod platform;
pub mod resolved;
pub mod utils;
pub mod validate;

pub use resolved::ResolvedApi;
