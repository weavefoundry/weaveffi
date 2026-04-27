use std::io::Write;

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

fn write(path: &std::path::Path, contents: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

#[test]
fn format_check_passes_for_canonical_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.yml");
    write(
        &path,
        concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params: []\n",
        ),
    );

    cargo_bin()
        .args(["format", path.to_str().unwrap()])
        .assert()
        .success();

    cargo_bin()
        .args(["format", path.to_str().unwrap(), "--check"])
        .assert()
        .success();
}

#[test]
fn format_rewrites_unsorted_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.yml");
    write(
        &path,
        concat!(
            "modules:\n",
            "  - functions:\n",
            "      - return: i32\n",
            "        params:\n",
            "          - { type: i32, name: a }\n",
            "          - { type: i32, name: b }\n",
            "        name: add\n",
            "    name: math\n",
            "version: \"0.1.0\"\n",
        ),
    );
    let original = std::fs::read_to_string(&path).unwrap();

    cargo_bin()
        .args(["format", path.to_str().unwrap()])
        .assert()
        .success();

    let formatted = std::fs::read_to_string(&path).unwrap();
    assert_ne!(formatted, original, "format should rewrite the file");

    let key_lines: Vec<&str> = formatted
        .lines()
        .filter(|l| {
            l.trim_start()
                .starts_with(|c: char| c.is_alphabetic() || c == '"')
                && l.contains(':')
        })
        .collect();
    assert!(
        key_lines.iter().any(|l| l.starts_with("modules:")),
        "expected top-level 'modules:' key, got: {key_lines:?}"
    );
    let modules_idx = key_lines
        .iter()
        .position(|l| l.starts_with("modules:"))
        .unwrap();
    let version_idx = key_lines
        .iter()
        .position(|l| l.starts_with("version:"))
        .unwrap();
    assert!(
        modules_idx < version_idx,
        "modules should be sorted before version at top level: {key_lines:?}"
    );

    cargo_bin()
        .args(["format", path.to_str().unwrap(), "--check"])
        .assert()
        .success();
}

#[test]
fn format_check_fails_for_unsorted_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.yml");
    write(
        &path,
        concat!(
            "modules:\n",
            "  - functions:\n",
            "      - return: i32\n",
            "        params: []\n",
            "        name: noop\n",
            "    name: math\n",
            "version: \"0.1.0\"\n",
        ),
    );
    let original = std::fs::read_to_string(&path).unwrap();

    let output = cargo_bin()
        .args(["format", path.to_str().unwrap(), "--check"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--check should exit non-zero for unsorted input: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original, "--check must not modify the input file");
}
