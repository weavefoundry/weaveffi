use std::path::Path;

use weaveffi_ir::ir::Api;

fn repo_root() -> &'static Path {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir).parent().unwrap().parent().unwrap()
}

fn load_calculator_api() -> Api {
    let yml = std::fs::read_to_string(repo_root().join("samples/calculator/calculator.yml"))
        .expect("failed to read calculator.yml");
    serde_yaml::from_str(&yml).expect("failed to parse calculator.yml")
}

#[test]
fn generate_from_json_input() {
    let api = load_calculator_api();
    let json = serde_json::to_string_pretty(&api).expect("failed to serialize to JSON");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let json_path = tmp.path().join("calculator.json");
    std::fs::write(&json_path, &json).expect("failed to write JSON file");

    let out_dir = tempfile::tempdir().expect("failed to create output dir");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            json_path.to_str().unwrap(),
            "-o",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        out_dir.path().join("c").exists(),
        "c/ output directory should exist"
    );
}

#[test]
fn generate_from_toml_input() {
    let api = load_calculator_api();
    let toml_str = toml::to_string_pretty(&api).expect("failed to serialize to TOML");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let toml_path = tmp.path().join("calculator.toml");
    std::fs::write(&toml_path, &toml_str).expect("failed to write TOML file");

    let out_dir = tempfile::tempdir().expect("failed to create output dir");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            toml_path.to_str().unwrap(),
            "-o",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        out_dir.path().join("c").exists(),
        "c/ output directory should exist"
    );
}

#[test]
fn validate_from_json() {
    let api = load_calculator_api();
    let json = serde_json::to_string_pretty(&api).expect("failed to serialize to JSON");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let json_path = tmp.path().join("calculator.json");
    std::fs::write(&json_path, &json).expect("failed to write JSON file");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["validate", json_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("Validation passed"));
}
