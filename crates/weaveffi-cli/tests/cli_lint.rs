use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

#[test]
fn lint_clean_calculator() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    cargo_bin()
        .args(["lint", input.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::prelude::predicate::str::contains(
            "No warnings.",
        ));
}

#[test]
fn lint_warns_on_undocumented() {
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

    let output = cargo_bin()
        .args(["lint", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "lint should fail when warnings are present"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning:"),
        "expected warning prefix in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("no doc comments"),
        "expected doc comment warning text, got: {stderr}"
    );
}
