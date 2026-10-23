//! Integration tests for the CI-oriented flags: `weaveffi generate --check`
//! and `weaveffi validate [--warn] --format json`. Each test runs the binary as a
//! subprocess and either asserts on the structured stdout or on the process
//! exit code.

use std::io::Write;
use std::path::Path;

fn cargo_bin() -> assert_cmd::Command {
    crate::weaveffi()
}

fn write_file(path: &Path, contents: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

fn calculator_crate() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples/calculator")
}

#[test]
fn check_passes_when_output_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("generated");
    let input = calculator_crate();

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
            "generate",
            input.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--check",
        ])
        .output()
        .expect("failed to run weaveffi generate --check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "generate --check should exit 0 when output matches; stdout={stdout}, stderr={stderr}"
    );
    assert!(stdout.is_empty(), "nothing would change, got: {stdout}");
    assert!(stderr.contains("is up to date"), "{stderr}");
}

#[test]
fn check_fails_when_idl_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("generated");
    let idl = tmp.path().join("api.yml");
    write_file(
        &idl,
        concat!(
            "version: \"0.12.0\"\n",
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
            "version: \"0.12.0\"\n",
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
            "generate",
            idl.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--check",
            "--quiet",
        ])
        .output()
        .expect("failed to run weaveffi generate --check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "generate --check should exit 1 when the IDL drifted; stdout={stdout}"
    );
    assert!(stdout.lines().any(|l| l == "~ c/api.h"), "{stdout}");
    assert!(
        !stdout.contains("---") && !stdout.contains("+++"),
        "generate --check must not print per-file diff content, got: {stdout}"
    );
    assert!(
        std::fs::read_to_string(out.join("c/api.h")).is_ok_and(|h| !h.contains("api_calc_sub")),
        "generate --check must not write"
    );
}

#[test]
fn validate_json_format_outputs_object() {
    let input = calculator_crate();

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
    assert!(parsed.get("records").is_some(), "missing 'records' key");
    assert!(parsed.get("enums").is_some(), "missing 'enums' key");
}

#[test]
fn validate_warn_json_format_outputs_warnings_array() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nodocs.yml");
    write_file(
        &path,
        concat!(
            "version: \"0.12.0\"\n",
            "modules:\n",
            "  - name: nodocs\n",
            "    functions:\n",
            "      - name: do_stuff\n",
            "        params: []\n",
        ),
    );

    let output = cargo_bin()
        .args([
            "validate",
            path.to_str().unwrap(),
            "--warn",
            "--format",
            "json",
            "--quiet",
        ])
        .output()
        .expect("failed to run weaveffi validate --warn --format json");

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
        serde_json::Value::Bool(true),
        "warnings are advisory, so a valid IDL still reports ok"
    );
    assert!(output.status.success(), "warnings must not fail validate");
}

#[test]
fn validate_json_failures_carry_the_code_and_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("global.yml");
    write_file(
        &path,
        concat!(
            "version: \"0.12.0\"\n",
            "modules:\n",
            "  - name: a\n",
            "    structs: [{ name: Item, fields: [{ name: n, type: usize }] }]\n",
            "    functions: [{ name: open, params: [{ name: i, type: a.Item }] }]\n",
            "  - name: b\n",
            "    functions: [{ name: open }]\n",
        ),
    );

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to run weaveffi validate --format json");
    assert!(!output.status.success(), "global name clashes must fail");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    let errors = parsed["errors"].as_array().expect("errors array");
    let find = |code: &str| {
        errors
            .iter()
            .find(|e| e["code"] == code)
            .unwrap_or_else(|| panic!("no {code} in {parsed}"))
    };
    assert_eq!(find("UnsupportedPrimitive")["name"], "usize");
    assert_eq!(find("QualifiedTypeRef")["name"], "a.Item");
    let dup = find("DuplicateFunctionName");
    assert_eq!(dup["name"], "open");
    assert_eq!(dup["first"], "a");
    assert_eq!(dup["second"], "b");
    assert!(dup["suggestion"].as_str().is_some_and(|s| !s.is_empty()));
}
