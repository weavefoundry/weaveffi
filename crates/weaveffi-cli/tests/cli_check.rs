//! Integration tests for the CI-oriented flags: `weaveffi diff --check` and
//! `weaveffi validate|lint --format json`. Each test runs the binary as a
//! subprocess and either asserts on the structured stdout or on the process
//! exit code.

use std::io::Write;
use std::path::Path;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

fn write_file(path: &Path, contents: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

fn calculator_idl() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples/calculator/calculator.yml")
}

#[test]
fn diff_check_passes_when_output_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("generated");
    let input = calculator_idl();

    cargo_bin()
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let output = cargo_bin()
        .args([
            "diff",
            input.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--check",
        ])
        .output()
        .expect("failed to run weaveffi diff --check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "diff --check should exit 0 when output matches; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("+ 0 added, - 0 removed, ~ 0 modified"),
        "expected zeroed summary, got: {stdout}"
    );
    assert!(
        !stdout.contains("---") && !stdout.contains("+++"),
        "diff --check must not print per-file diff content, got: {stdout}"
    );
}

#[test]
fn diff_check_fails_when_idl_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("generated");
    let idl = tmp.path().join("api.yml");
    write_file(
        &idl,
        concat!(
            "version: \"0.3.0\"\n",
            "modules:\n",
            "  - name: calc\n",
            "    functions:\n",
            "      - name: add\n",
            "        doc: Add two integers\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
        ),
    );

    cargo_bin()
        .args([
            "generate",
            idl.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    write_file(
        &idl,
        concat!(
            "version: \"0.3.0\"\n",
            "modules:\n",
            "  - name: calc\n",
            "    functions:\n",
            "      - name: add\n",
            "        doc: Add two integers\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "      - name: sub\n",
            "        doc: Subtract two integers\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
        ),
    );

    let output = cargo_bin()
        .args([
            "diff",
            idl.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--check",
        ])
        .output()
        .expect("failed to run weaveffi diff --check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "diff --check should fail when IDL drifted; stdout={stdout}"
    );
    let code = output.status.code().expect("expected an exit code");
    assert!(
        code == 2 || code == 3,
        "expected exit code 2 (modified) or 3 (added/removed), got {code}; stdout={stdout}"
    );
    assert!(
        stdout.contains(" added, ")
            && stdout.contains(" removed, ")
            && stdout.contains(" modified"),
        "expected diff summary line, got: {stdout}"
    );
    assert!(
        !stdout.contains("---") && !stdout.contains("+++"),
        "diff --check must not print per-file diff content, got: {stdout}"
    );
}

#[test]
fn validate_json_format_outputs_object() {
    let input = calculator_idl();

    let output = cargo_bin()
        .args(["validate", input.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to run weaveffi validate --format json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "validate should succeed for the calculator sample; stdout={stdout}, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
    assert_eq!(parsed["modules"], serde_json::Value::from(1));
    assert!(
        parsed["functions"].as_u64().unwrap() >= 1,
        "expected at least 1 function in calculator sample, got: {parsed}"
    );
    assert!(parsed.get("structs").is_some(), "missing 'structs' key");
    assert!(parsed.get("enums").is_some(), "missing 'enums' key");
}

#[test]
fn lint_json_format_outputs_warnings_array() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nodocs.yml");
    write_file(
        &path,
        concat!(
            "version: \"0.3.0\"\n",
            "modules:\n",
            "  - name: nodocs\n",
            "    functions:\n",
            "      - name: do_stuff\n",
            "        params: []\n",
        ),
    );

    let output = cargo_bin()
        .args([
            "lint",
            path.to_str().unwrap(),
            "--format",
            "json",
            "--quiet",
        ])
        .output()
        .expect("failed to run weaveffi lint --format json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");

    let warnings = parsed["warnings"]
        .as_array()
        .expect("expected 'warnings' to be an array");
    assert!(
        !warnings.is_empty(),
        "expected at least one warning for an undocumented module, got: {parsed}"
    );
    let first = &warnings[0];
    assert!(first.get("code").is_some(), "warning missing 'code'");
    assert!(
        first.get("location").is_some(),
        "warning missing 'location'"
    );
    assert!(first.get("message").is_some(), "warning missing 'message'");

    assert_eq!(
        parsed["ok"],
        serde_json::Value::Bool(false),
        "ok should be false when warnings are present"
    );
}
