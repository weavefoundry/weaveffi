//! `weaveffi.toml` behavior end to end: automatic discovery next to (or
//! above) the input, `--config` overriding discovery, `[package]` identity
//! reaching every manifest, `[generators.<target>]` options reaching their
//! backend, and `[global] c_prefix` reaching all eleven targets.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Recursively read every file under `dir` and concatenate the text contents.
fn read_tree(dir: &Path) -> String {
    let mut out = String::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&read_tree(&path));
        } else if let Ok(contents) = fs::read_to_string(&path) {
            out.push_str(&contents);
            out.push('\n');
        }
    }
    out
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()))
}

fn generate(input: &Path, out: &Path, extra: &[&str]) {
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .args(extra)
        .assert()
        .success();
}

const ALL_TARGETS: [&str; 11] = [
    "c", "cpp", "swift", "kotlin", "node", "wasm", "python", "dotnet", "dart", "go", "ruby",
];

/// The kvstore sample's `weaveffi.toml` sits beside its `src/lib.rs`; running
/// on the Rust source must pick it up without `--config` and honor every
/// per-target table plus the `[package]` identity.
#[test]
fn discovers_weaveffi_toml_above_the_input() {
    let input = repo_root().join("samples/kvstore/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path();
    generate(&input, out, &[]);

    for target in ALL_TARGETS {
        assert!(
            out.join(target).is_dir(),
            "missing target directory: {target}"
        );
    }

    assert!(read(&out.join("cpp/weaveffi.hpp")).contains("namespace kvstore"));
    assert!(read(&out.join("dotnet/Kvstore.cs")).contains("namespace Kvstore"));
    assert!(out.join("swift/Sources/Kvstore/Kvstore.swift").is_file());
    assert!(read(&out.join("dart/pubspec.yaml")).contains("name: kvstore"));
    assert!(read(&out.join("go/go.mod")).contains("github.com/example/kvstore"));
    assert!(read(&out.join("ruby/lib/kvstore.rb")).contains("module Kvstore"));

    // `[package]` identity flows into the ecosystem manifests.
    let package_json = read(&out.join("node/package.json"));
    assert!(
        package_json.contains("\"name\": \"kvstore\""),
        "{package_json}"
    );
    assert!(
        package_json.contains("\"version\": \"1.0.0\""),
        "{package_json}"
    );
    assert!(
        package_json.contains("\"license\": \"MIT\""),
        "{package_json}"
    );
    let pyproject = read(&out.join("python/pyproject.toml"));
    assert!(pyproject.contains("name = \"kvstore\""), "{pyproject}");
    assert!(pyproject.contains("version = \"1.0.0\""), "{pyproject}");
}

/// `--config` names the file explicitly and wins over discovery.
#[test]
fn explicit_config_overrides_discovery() {
    let input = repo_root().join("samples/kvstore/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("other.toml");
    fs::write(
        &cfg,
        concat!(
            "[package]\n",
            "name = \"renamed\"\n",
            "version = \"9.9.9\"\n",
            "[generators.cpp]\n",
            "namespace = \"elsewhere\"\n",
        ),
    )
    .unwrap();
    let out = dir.path().join("out");
    generate(
        &input,
        &out,
        &["--target", "cpp,node", "--config", cfg.to_str().unwrap()],
    );

    assert!(read(&out.join("cpp/weaveffi.hpp")).contains("namespace elsewhere"));
    let package_json = read(&out.join("node/package.json"));
    assert!(
        package_json.contains("\"name\": \"renamed\""),
        "{package_json}"
    );
    assert!(
        package_json.contains("\"version\": \"9.9.9\""),
        "{package_json}"
    );
}

/// Without any config, a `lib.rs` input names its package after the crate
/// directory (Cargo's convention), not after the file stem.
#[test]
fn lib_rs_without_config_is_named_after_its_crate_directory() {
    let dir = tempfile::tempdir().unwrap();
    let crate_dir = dir.path().join("mycrate").join("src");
    fs::create_dir_all(&crate_dir).unwrap();
    let input = crate_dir.join("lib.rs");
    fs::copy(repo_root().join("samples/calculator/src/lib.rs"), &input).unwrap();
    let out = dir.path().join("out");
    generate(&input, &out, &["--target", "node"]);
    let package_json = read(&out.join("node/package.json"));
    assert!(
        package_json.contains("\"name\": \"mycrate\""),
        "{package_json}"
    );
}

/// A stale `package:` or `generators:` block in an IDL document is rejected
/// with a message that names the offending key, rather than being silently
/// ignored.
#[test]
fn inline_package_block_in_idl_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let idl = dir.path().join("api.yml");
    fs::write(
        &idl,
        concat!(
            "version: \"0.9.0\"\n",
            "package:\n",
            "  name: legacy\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
        ),
    )
    .unwrap();
    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["validate", idl.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("package"), "{stderr}");
}

/// Unknown top-level tables in `weaveffi.toml` (such as the pre-0.8 `[swift]`
/// spelling) fail loudly so a misplaced option cannot be silently dropped.
#[test]
fn unknown_config_table_is_an_error() {
    let input = repo_root().join("samples/calculator/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "[swift]\nmodule_name = \"Old\"\n").unwrap();
    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("out").to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("swift"), "{stderr}");
    assert!(stderr.contains("failed to parse config file"), "{stderr}");
}

/// An unknown `--target` fails instead of silently generating nothing.
#[test]
fn unknown_target_is_an_error() {
    let input = repo_root().join("samples/calculator/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            dir.path().join("out").to_str().unwrap(),
            "--target",
            "c,rustlang",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rustlang"), "{stderr}");
}

/// The C ABI prefix must reach **every** language backend, not just C/C++:
/// a custom prefix otherwise produces consumer code that links against
/// symbols the (re-prefixed) producer never exported. Generates all targets
/// with `[global] c_prefix = "myffi"` and asserts each one emits the prefixed
/// user symbol and never the default-prefixed one.
#[test]
fn global_c_prefix_propagates_to_all_targets() {
    let input = repo_root().join("samples/calculator/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "[global]\nc_prefix = \"myffi\"\n").unwrap();
    generate(&input, &out, &["--config", cfg.to_str().unwrap()]);

    for target in ALL_TARGETS {
        let tree = read_tree(&out.join(target));
        assert!(
            tree.contains("myffi_calculator_add"),
            "target `{target}` did not honor the custom c_prefix"
        );
        assert!(
            !tree.contains("weaveffi_calculator_add"),
            "target `{target}` leaked the default-prefixed user symbol"
        );
    }
}

/// `[generators.c] prefix` alone also fans out (to C++ at least), and the C
/// header aliases the runtime symbols to the custom prefix.
#[test]
fn c_prefix_from_the_c_table_reaches_cpp() {
    let input = repo_root().join("samples/calculator/src/lib.rs");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "[generators.c]\nprefix = \"myffi\"\n").unwrap();
    generate(
        &input,
        &out,
        &["--target", "c,cpp", "--config", cfg.to_str().unwrap()],
    );

    let header = read(&out.join("c/myffi.h"));
    for alias in [
        "#define myffi_error weaveffi_error",
        "#define myffi_error_set weaveffi_error_set",
        "#define myffi_free_string weaveffi_free_string",
        "#define myffi_cancel_token_create weaveffi_cancel_token_create",
    ] {
        assert!(
            header.contains(alias),
            "header missing `{alias}`:\n{header}"
        );
    }
    assert!(header.contains("myffi_calculator_add"));
    assert!(!header.contains("weaveffi_calculator_add"));

    let cpp = read(&out.join("cpp/weaveffi.hpp"));
    assert!(cpp.contains("extern \"C\" {"));
    assert!(cpp.contains("myffi_calculator_add"));
    assert!(!cpp.contains("weaveffi_calculator_add"));
    assert!(cpp.contains("#define myffi_error weaveffi_error"));
}
