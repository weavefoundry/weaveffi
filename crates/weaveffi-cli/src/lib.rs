//! The library behind the `weaveffi` command-line tool: the
//! [`Project`](project::Project) that locates a project and loads its API
//! (from an IDL, or from a Rust producer's built [`library`]), the
//! `weaveffi.toml` [`config`], the [`LanguageBackend`](backend::LanguageBackend)
//! trait, the code-generation [`Orchestrator`](codegen::Orchestrator), the
//! shared C ABI declaration renderer, the eleven language generators under
//! [`targets`], and the [`build`] and [`package`] layers that turn a producer
//! crate into installable per-ecosystem artifacts.
//!
//! Every generator renders from the validated
//! [`Model`](weaveffi_model::model::Model) alone, so symbol names and
//! parameter lowering are computed once and shared.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

pub mod backend;
pub mod build;
pub mod cabi;
pub mod cache;
pub mod cargo;
pub mod codegen;
pub mod config;
pub mod lang;
pub mod library;
pub mod manifest;
pub mod package;
pub mod platform;
pub mod project;
mod report;
pub mod targets;
pub mod utils;
