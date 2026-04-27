use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

fn write_yaml(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    path
}

#[test]
fn upgrade_from_v0_1_0_writes_v0_3_0() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "version: \"0.1.0\"\nmodules:\n  - name: math\n    functions:\n      - name: add\n        params:\n          - { name: a, type: i32 }\n          - { name: b, type: i32 }\n        return: i32\n";
    let path = write_yaml(dir.path(), "api.yml", yaml);

    let output = cargo_bin()
        .args(["upgrade", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "upgrade failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let new_contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        new_contents.contains("version: 0.3.0") || new_contents.contains("version: \"0.3.0\""),
        "expected migrated version 0.3.0, got: {new_contents}"
    );
    assert!(
        !new_contents.contains("0.1.0"),
        "old version remains in: {new_contents}"
    );
}

#[test]
fn upgrade_already_current_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "version: \"0.3.0\"\nmodules:\n  - name: math\n    functions:\n      - name: add\n        params:\n          - { name: a, type: i32 }\n        return: i32\n";
    let path = write_yaml(dir.path(), "api.yml", yaml);
    let original = std::fs::read_to_string(&path).unwrap();

    let output = cargo_bin()
        .args(["upgrade", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "upgrade noop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Already up to date (version 0.3.0)"),
        "expected 'Already up to date' message, got: {stdout}"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(original, after, "file should be untouched");
}

#[test]
fn upgrade_check_mode_exits_nonzero_when_outdated() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "version: \"0.1.0\"\nmodules:\n  - name: math\n    functions:\n      - name: add\n        params: []\n        return: i32\n";
    let path = write_yaml(dir.path(), "api.yml", yaml);
    let original = std::fs::read_to_string(&path).unwrap();

    let output = cargo_bin()
        .args(["upgrade", path.to_str().unwrap(), "--check"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "expected non-zero exit");
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit code 2, got: {:?}",
        output.status.code()
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(original, after, "--check should not write the file");
}

#[test]
fn upgrade_unsupported_version_errors() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "version: \"9.9.9\"\nmodules:\n  - name: math\n    functions: []\n";
    let path = write_yaml(dir.path(), "api.yml", yaml);

    let output = cargo_bin()
        .args(["upgrade", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected error for unsupported version"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported source version"),
        "expected unsupported version error, got: {stderr}"
    );
}
