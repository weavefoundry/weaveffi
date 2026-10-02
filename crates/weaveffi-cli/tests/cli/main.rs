//! Integration tests for the `weaveffi` binary and the CLI-facing pipeline,
//! compiled as one test binary (one link step instead of one per file). The
//! per-generator snapshot corpus lives separately in `tests/snapshots.rs`.

mod check;
mod determinism;
mod diagnostics;
mod diff;
mod errors;
mod extract;
mod extract_roundtrip;
mod formats;
mod generate;
mod no_silent_stubs;
mod package;
mod project_config;
mod schema;
