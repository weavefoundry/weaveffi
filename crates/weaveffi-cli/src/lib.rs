//! The library behind the `weaveffi` command-line tool: the
//! [`Project`](project::Project) that locates a project and loads its API
//! (from an IDL, or from a Rust producer's built library), the
//! `weaveffi.toml` [`config`], the language [`targets`] with their
//! [`REGISTRY`](targets::REGISTRY), the code-generation
//! [`Orchestrator`](codegen::Orchestrator), and the [`package`] artifacts
//! `weaveffi package` writes.
//!
//! Every target renders from the validated
//! [`Model`](weaveffi_model::model::Model) alone, so symbol names and
//! parameter lowering are computed once and shared.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

pub(crate) mod build;
pub(crate) mod cabi;
pub(crate) mod cargo;
pub mod codegen;
#[doc(hidden)]
pub mod commands;
pub mod config;
pub(crate) mod lang;
pub(crate) mod library;
pub(crate) mod manifest;
pub mod package;
pub(crate) mod platform;
pub mod project;
pub(crate) mod record;
mod report;
pub mod targets;
pub(crate) mod utils;
