use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

/// With a well-documented, clean IDL, `check` should succeed and produce
/// output from both the validate step (the "Validation passed" summary) and
/// the lint step (the "No warnings." line).
#[test]
fn check_runs_both_validate_and_lint() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: clean
    doc: Clean module.
    functions:
      - name: do_stuff
        doc: Do some stuff.
        params: []
"#;
    let path = dir.path().join("clean.yml");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(yaml.as_bytes()).unwrap();

    let output = cargo_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "check should succeed on a clean IDL; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Validation passed"),
        "expected validate summary in stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("No warnings."),
        "expected lint summary in stdout, got: {stdout}"
    );
}

/// `check --strict` must exit non-zero when lint emits any warning, even
/// though validation itself passes.
#[test]
fn check_strict_fails_on_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: nodocs
    functions:
      - name: do_stuff
        params: []
"#;
    let path = dir.path().join("nodocs.yml");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(yaml.as_bytes()).unwrap();

    let without_strict = cargo_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        without_strict.status.success(),
        "plain `check` should exit 0 even when lint warnings exist; stderr: {}",
        String::from_utf8_lossy(&without_strict.stderr)
    );
    let without_strict_stderr = String::from_utf8_lossy(&without_strict.stderr);
    assert!(
        without_strict_stderr.contains("warning:"),
        "warnings should still be reported to stderr without --strict, got: {without_strict_stderr}"
    );

    let with_strict = cargo_bin()
        .args(["check", "--strict", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !with_strict.status.success(),
        "check --strict must exit non-zero when warnings are present"
    );
    let with_strict_stderr = String::from_utf8_lossy(&with_strict.stderr);
    assert!(
        with_strict_stderr.contains("warning:"),
        "expected warning prefix in stderr, got: {with_strict_stderr}"
    );
}
