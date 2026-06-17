//! CI gate: no silent stubs, in either the generators or their output.
//!
//! The pre-overhaul backends shipped bindings that compiled but lied: builder
//! `build()` methods that threw "requires FFI backing", `unimplemented!()`
//! paths for whole call shapes, features skipped without a word. These tests
//! make that class of regression a build failure:
//!
//! 1. Generator crate sources must not contain `unimplemented!(` / `todo!(`.
//!    (`unreachable!` stays allowed: it documents genuinely impossible states.)
//! 2. The full generated output for the feature-complete sample IDLs must not
//!    contain stub markers. A target that cannot support a feature must either
//!    fail generation loudly (the capability gate) or emit an *explicit*
//!    "not supported by this target" surface, never a fake implementation.

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Generator crate sources must not punt with panicking placeholder macros.
#[test]
fn generator_sources_ban_unimplemented_and_todo() {
    let crates_dir = workspace_root().join("crates");
    let mut scanned = 0usize;
    let mut violations: Vec<String> = Vec::new();

    for entry in fs::read_dir(&crates_dir).expect("read crates/") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The CLI's `scaffold` output intentionally emits `todo!()` for the
        // *user's* Rust producer skeleton, so only generator + core crates
        // (the code that produces consumer bindings) are in scope.
        if !(name.starts_with("weaveffi-gen-") || name == "weaveffi-core") {
            continue;
        }
        let src = path.join("src");
        let mut files = Vec::new();
        walk_files(&src, &mut files);
        for file in files {
            if file.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = fs::read_to_string(&file).expect("read source file");
            scanned += 1;
            for banned in ["unimplemented!(", "todo!("] {
                if text.contains(banned) {
                    violations.push(format!("{}: contains `{banned}`", file.display()));
                }
            }
        }
    }

    assert!(
        scanned > 10,
        "expected to scan generator sources, got {scanned} files"
    );
    assert!(
        violations.is_empty(),
        "panicking placeholder macros in generator crates (implement the path, \
         fail generation via the capability gate, or use unreachable! for \
         impossible states):\n{}",
        violations.join("\n")
    );
}

/// Every file generated for the feature-complete samples (structs, builders,
/// enums, optionals, lists, maps, bytes, handles, callbacks, listeners, async,
/// iterators, submodules) must be free of stub markers across all targets.
#[test]
fn generated_output_has_no_stub_markers() {
    let root = workspace_root();
    let samples = [
        root.join("samples/contacts/contacts.yml"),
        root.join("samples/events/events.yml"),
        root.join("samples/kvstore/kvstore.yml"),
    ];

    // Case-insensitive marker list. "not supported" is deliberately absent:
    // explicit unsupported-feature stubs (e.g. wasm listeners behind
    // allow_unsupported) are the *correct* loud behavior.
    let banned = [
        "unimplemented",
        "notimplemented",
        "not implemented",
        "requires ffi backing",
    ];

    let mut violations: Vec<String> = Vec::new();
    for idl in &samples {
        assert!(idl.exists(), "missing sample IDL: {}", idl.display());
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out");

        assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args([
                "generate",
                idl.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--force",
            ])
            .assert()
            .success();

        let mut files = Vec::new();
        walk_files(&out, &mut files);
        assert!(
            files.len() > 10,
            "expected many generated files for {}, got {}",
            idl.display(),
            files.len()
        );
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue; // non-UTF-8 artifacts have no text stubs to check
            };
            let lower = text.to_lowercase();
            for marker in banned {
                if lower.contains(marker) {
                    violations.push(format!(
                        "{} (from {}): contains \"{marker}\"",
                        file.display(),
                        idl.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "stub markers found in generated output:\n{}",
        violations.join("\n")
    );
}
