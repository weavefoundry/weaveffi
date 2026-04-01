use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

fn write_temp_file(dir: &tempfile::TempDir, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    path
}

#[test]
fn parse_error_yaml_shows_filename_and_location() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_file(&dir, "bad.yml", "version: [invalid\n");

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("bad.yml"),
        "expected filename in error output, got: {stderr}"
    );
    assert!(
        stderr.contains("YAML parse error"),
        "expected YAML parse error message, got: {stderr}"
    );
    assert!(
        stderr.contains("Suggestion"),
        "expected Suggestion section, got: {stderr}"
    );
}

#[test]
fn parse_error_json_shows_filename_and_location() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_file(&dir, "bad.json", "{ invalid json }");

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("bad.json"),
        "expected filename in error output, got: {stderr}"
    );
    assert!(
        stderr.contains("JSON parse error"),
        "expected JSON parse error message, got: {stderr}"
    );
}

#[test]
fn parse_error_toml_shows_filename() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_file(&dir, "bad.toml", "version = [invalid\n");

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("bad.toml"),
        "expected filename in error output, got: {stderr}"
    );
    assert!(
        stderr.contains("TOML parse error"),
        "expected TOML parse error message, got: {stderr}"
    );
}

#[test]
fn validation_error_duplicate_module_shows_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: foo
    functions:
      - name: bar
        params: []
  - name: foo
    functions:
      - name: baz
        params: []
"#;
    let path = write_temp_file(&dir, "dup.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("duplicate module name"),
        "expected rule violation message, got: {stderr}"
    );
    assert!(
        stderr.contains("Suggestion"),
        "expected Suggestion section, got: {stderr}"
    );
    assert!(
        stderr.contains("module names must be unique"),
        "expected fix suggestion text, got: {stderr}"
    );
}

#[test]
fn validation_error_async_shows_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: mymod
    functions:
      - name: do_stuff
        params: []
        async: true
"#;
    let path = write_temp_file(&dir, "async.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("async functions are not yet supported"),
        "expected async rejection message, got: {stderr}"
    );
    assert!(
        stderr.contains("remove 'async: true'"),
        "expected fix suggestion for async, got: {stderr}"
    );
}

#[test]
fn validation_error_duplicate_function_shows_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: mymod
    functions:
      - name: do_stuff
        params: []
      - name: do_stuff
        params: []
"#;
    let path = write_temp_file(&dir, "dupfn.yml", yaml);

    let output = cargo_bin()
        .args(["validate", path.to_str().unwrap()])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("duplicate function name"),
        "expected rule violation message, got: {stderr}"
    );
    assert!(
        stderr.contains("function names must be unique"),
        "expected fix suggestion text, got: {stderr}"
    );
}

#[test]
fn generate_parse_error_shows_filename_and_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_file(&dir, "broken.yml", "not: valid: yaml: [");
    let out_dir = dir.path().join("out");

    let output = cargo_bin()
        .args([
            "generate",
            path.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("broken.yml"),
        "expected filename in generate error, got: {stderr}"
    );
    assert!(
        stderr.contains("Suggestion"),
        "expected Suggestion section in generate error, got: {stderr}"
    );
}

#[test]
fn generate_validation_error_shows_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: dup
    functions:
      - name: f
        params: []
  - name: dup
    functions:
      - name: g
        params: []
"#;
    let path = write_temp_file(&dir, "dup_gen.yml", yaml);
    let out_dir = dir.path().join("out");

    let output = cargo_bin()
        .args([
            "generate",
            path.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("duplicate module name"),
        "expected rule violation in generate, got: {stderr}"
    );
    assert!(
        stderr.contains("Suggestion"),
        "expected Suggestion section in generate, got: {stderr}"
    );
}

#[test]
fn validate_with_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
version: "0.1.0"
modules:
  - name: nodocs
    functions:
      - name: do_stuff
        params: []
"#;
    let path = write_temp_file(&dir, "nodocs.yml", yaml);

    let output = cargo_bin()
        .args(["validate", "--warn", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(output.status.success(), "validate should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning:"),
        "expected warning prefix in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("nodocs"),
        "expected module name in warning, got: {stderr}"
    );
    assert!(
        stderr.contains("no doc comments"),
        "expected doc comment warning text, got: {stderr}"
    );
}
