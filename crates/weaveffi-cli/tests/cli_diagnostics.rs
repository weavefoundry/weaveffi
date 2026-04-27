//! End-to-end diagnostic tests asserting that the `weaveffi` binary surfaces
//! span-aware error output produced by `miette`. Each test runs the binary as
//! a subprocess and inspects rendered stderr (with colors disabled) for the
//! offending identifier and line/column information that the new diagnostic
//! infrastructure is supposed to produce.

use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found");
    cmd.env("NO_COLOR", "1");
    cmd
}

fn write_temp_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    path
}

/// A YAML file with a syntax error on a known line should produce an error
/// whose rendered stderr includes that line number, the originating filename,
/// and an underlined snippet from the source.
#[test]
fn parse_yaml_error_includes_line() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "version: \"0.1.0\"\nmodules:\n  - name: [bad\n";
    let path = write_temp_file(&dir, "broken.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success(), "expected validate to fail");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("broken.yml"),
        "expected filename in diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("YAML parse error"),
        "expected YAML parse error message, got:\n{stderr}"
    );
    assert!(
        stderr.contains("line 3"),
        "expected the offending line number (3) in diagnostic, got:\n{stderr}"
    );
}

/// A YAML file whose body validates structurally but references an unknown
/// type should produce a `ValidationError::UnknownTypeRef` diagnostic that
/// names the offending identifier in the rendered output.
#[test]
fn validate_unknown_typeref_includes_offending_name() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"version: "0.1.0"
modules:
  - name: m
    functions:
      - name: f
        params:
          - name: x
            type: NotARealType
"#;
    let path = write_temp_file(&dir, "unknown.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success(), "expected validate to fail");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("NotARealType"),
        "expected offending identifier 'NotARealType' in diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("unknown.yml"),
        "expected filename in diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("help:"),
        "expected miette 'help:' section, got:\n{stderr}"
    );
}

/// A duplicate module name should be flagged with the offending name in the
/// rendered diagnostic, along with the filename.
#[test]
fn validate_duplicate_module_includes_offending_name() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"version: "0.1.0"
modules:
  - name: alpha
    functions:
      - name: f
        params: []
  - name: alpha
    functions:
      - name: g
        params: []
"#;
    let path = write_temp_file(&dir, "dup.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success(), "expected validate to fail");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("alpha"),
        "expected offending module name 'alpha' in diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("duplicate module name"),
        "expected duplicate module name message, got:\n{stderr}"
    );
    assert!(
        stderr.contains("dup.yml"),
        "expected filename in diagnostic, got:\n{stderr}"
    );
}

/// A JSON file with a syntax error should produce a diagnostic whose rendered
/// stderr includes the offending line number and filename.
#[test]
fn parse_json_error_includes_line() {
    let dir = tempfile::tempdir().unwrap();
    let json = "{\n  \"version\": \"0.1.0\",\n  \"modules\": [ broken\n}\n";
    let path = write_temp_file(&dir, "broken.json", json);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success(), "expected validate to fail");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("broken.json"),
        "expected filename in diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("JSON parse error"),
        "expected JSON parse error message, got:\n{stderr}"
    );
    assert!(
        stderr.contains("line 3"),
        "expected the offending line number (3) in diagnostic, got:\n{stderr}"
    );
}
