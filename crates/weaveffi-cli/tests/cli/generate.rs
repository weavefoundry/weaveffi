use predicates::prelude::*;
use std::path::Path;

#[test]
fn generate_produces_expected_files() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    crate::weaveffi()
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    for file in [
        "c/calculator.h",
        "cpp/calculator.hpp",
        "swift/Package.swift",
        "swift/Sources/Calculator/Calculator.swift",
        "kotlin/build.gradle.kts",
        "node/index.d.ts",
        "wasm/index.js",
        "python/calculator/calculator.py",
        "dotnet/Calculator.cs",
        "dart/lib/calculator.dart",
        "go/go.mod",
        "ruby/lib/calculator.rb",
    ] {
        assert!(out_path.join(file).exists(), "missing {file}");
    }
}

#[test]
fn generate_with_target_filter() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    crate::weaveffi()
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "c",
        ])
        .assert()
        .success();

    assert!(
        out_path.join("c/calculator.h").exists(),
        "missing c/calculator.h"
    );
    assert!(
        !out_path.join("swift").exists(),
        "swift/ should not exist when --target c is used"
    );
}

#[test]
fn generate_cpp_target_filter() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    crate::weaveffi()
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "cpp",
        ])
        .assert()
        .success();

    assert!(
        out_path.join("cpp").exists(),
        "cpp/ should exist when --target cpp is used"
    );
    assert!(
        !out_path.join("c").exists(),
        "c/ should not exist when --target cpp is used"
    );
}

#[test]
fn validate_command_succeeds() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");

    crate::weaveffi()
        .args(["validate", input.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("Validation passed"));
}

#[test]
fn quiet_flag_suppresses_output() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    crate::weaveffi()
        .args([
            "--quiet",
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    assert!(
        out_path.join("c/calculator.h").exists(),
        "files should still be generated with --quiet"
    );
}

/// `weaveffi dev` builds the debug library, generates, and copies the
/// library into the Python package, which loads a bundled copy first; the
/// targets that can't bundle it are told which variable to set.
#[test]
fn dev_bundles_the_debug_library_into_the_python_package() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator");
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    let output = crate::weaveffi()
        .args(["dev", input.to_str().unwrap(), "-o"])
        .arg(out_path)
        .args(["--target", "c,python,dotnet"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let library = if cfg!(target_os = "macos") {
        "libcalculator.dylib"
    } else if cfg!(windows) {
        "calculator.dll"
    } else {
        "libcalculator.so"
    };
    assert!(
        out_path.join("python/calculator").join(library).is_file(),
        "{stdout}"
    );
    assert!(
        stdout.contains("dotnet: export CALCULATOR_LIBRARY="),
        "{stdout}"
    );
    assert!(stdout.contains("c: link with"), "{stdout}");
}
