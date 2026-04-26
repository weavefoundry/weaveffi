//! Integration tests for the `weaveffi format` command.

const UNSORTED_YAML: &str = "\
version: \"0.1.0\"
modules:
  - name: zoo
    functions:
      - name: roar
        params: []
  - name: aardvark
    functions:
      - name: sleep
        params: []
      - name: dig
        params: []
    structs:
      - name: Burrow
        fields:
          - { name: width, type: i32 }
          - { name: depth, type: i32 }
";

#[test]
fn format_canonicalises_module_order() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let path = tmp.path().join("api.yml");
    std::fs::write(&path, UNSORTED_YAML).expect("failed to write input");

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["format", path.to_str().unwrap()])
        .output()
        .expect("failed to run format");

    assert!(
        output.status.success(),
        "format command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout not utf8");
    let aardvark_pos = stdout
        .find("name: aardvark")
        .expect("aardvark module missing from output");
    let zoo_pos = stdout
        .find("name: zoo")
        .expect("zoo module missing from output");
    assert!(
        aardvark_pos < zoo_pos,
        "modules should be sorted (aardvark before zoo):\n{stdout}"
    );

    let dig_pos = stdout.find("name: dig").expect("dig function missing");
    let sleep_pos = stdout.find("name: sleep").expect("sleep function missing");
    assert!(
        dig_pos < sleep_pos,
        "functions within a module should be sorted by name:\n{stdout}"
    );

    let depth_pos = stdout.find("name: depth").expect("depth field missing");
    let width_pos = stdout.find("name: width").expect("width field missing");
    assert!(
        depth_pos < width_pos,
        "struct fields should be sorted by name:\n{stdout}"
    );

    let input_after = std::fs::read_to_string(&path).expect("failed to re-read input");
    assert_eq!(
        input_after, UNSORTED_YAML,
        "format without --write must not modify the input file",
    );
}

#[test]
fn format_check_detects_non_canonical() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let path = tmp.path().join("api.yml");
    std::fs::write(&path, UNSORTED_YAML).expect("failed to write input");

    let check_first = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["format", "--check", path.to_str().unwrap()])
        .output()
        .expect("failed to run format --check");
    assert!(
        !check_first.status.success(),
        "--check on non-canonical input should exit non-zero"
    );
    assert_eq!(
        check_first.status.code(),
        Some(1),
        "--check should exit with code 1, got {:?}",
        check_first.status.code()
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("failed to re-read input"),
        UNSORTED_YAML,
        "--check must not modify the input file",
    );

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["format", "--write", path.to_str().unwrap()])
        .assert()
        .success();

    let check_second = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["format", "--check", path.to_str().unwrap()])
        .output()
        .expect("failed to run format --check after --write");
    assert!(
        check_second.status.success(),
        "--check should pass after --write; stderr: {}",
        String::from_utf8_lossy(&check_second.stderr)
    );
}
