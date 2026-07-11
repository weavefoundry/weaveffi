//! Core logic: Generator trait, codegen orchestration, validation, and shared utilities.
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
pub mod model;
pub mod package;
pub mod plan;
pub mod pkg;
pub mod platform;
pub mod utils;
pub mod validate;
