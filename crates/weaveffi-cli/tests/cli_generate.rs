use std::path::Path;

#[test]
fn generate_produces_expected_files() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        out_path.join("c/weaveffi.h").exists(),
        "missing c/weaveffi.h"
    );
    assert!(
        out_path.join("swift/Package.swift").exists(),
        "missing swift/Package.swift"
    );
    assert!(
        out_path.join("android/build.gradle").exists(),
        "missing android/build.gradle"
    );
    assert!(
        out_path.join("node/types.d.ts").exists(),
        "missing node/types.d.ts"
    );
    assert!(
        out_path.join("wasm/README.md").exists(),
        "missing wasm/README.md"
    );
}

#[test]
fn generate_with_target_filter() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
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
        out_path.join("c/weaveffi.h").exists(),
        "missing c/weaveffi.h"
    );
    assert!(
        !out_path.join("swift").exists(),
        "swift/ should not exist when --target c is used"
    );
}
