//! Integration tests for the `weaveffi` binary and the CLI-facing pipeline,
//! compiled as one test binary (one link step instead of one per file). The
//! per-generator snapshot corpus lives separately in `tests/snapshots.rs`.

mod check;
mod determinism;
mod diagnostics;
mod diff;
mod errors;
mod extract;
mod formats;
mod generate;
mod no_silent_stubs;
mod package;
mod project_config;
mod registry;
mod schema;

/// The `weaveffi` binary, building Rust producers into a target directory of
/// this test binary's own (`CARGO_TARGET_DIR`), so every library a test
/// reads is one a `weaveffi` command built for it, never an artifact another
/// test binary (or `cargo test` itself, with other features) wrote or is
/// rewriting in the workspace's `target/debug`.
fn weaveffi() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found");
    cmd.env(
        "CARGO_TARGET_DIR",
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-producers"),
    );
    cmd
}

/// A command's stderr with miette's wrapping undone: box-drawing gutters
/// removed and whitespace runs collapsed, so a message matches as one line
/// whatever width the report was wrapped to.
fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .replace(['│', '×'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
