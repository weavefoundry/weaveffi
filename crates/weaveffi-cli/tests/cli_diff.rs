use std::path::Path;

use predicates::prelude::*;

#[test]
fn diff_against_empty_dir() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let empty_out = tmp.path().join("empty");
    std::fs::create_dir_all(&empty_out).unwrap();

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "diff",
            input.to_str().unwrap(),
            "--out",
            empty_out.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run weaveffi diff");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "diff command failed: {stdout}");
    assert!(!stdout.is_empty(), "diff output should not be empty");

    for line in stdout.lines() {
        assert!(
            line.contains("[new file]"),
            "expected every line to contain [new file], got: {line}"
        );
    }
}

#[test]
fn diff_no_changes() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = tmp.path().join("generated");

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

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "diff",
            input.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No differences found."));
}
