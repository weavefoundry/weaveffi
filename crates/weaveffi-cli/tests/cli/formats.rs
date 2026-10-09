//! The JSON and TOML spellings of an IDL generate and validate like YAML.

use weaveffi_model::ir::Api;

/// The calculator sample's API, read from YAML.
fn load_calculator_api() -> Api {
    weaveffi_model::parse::parse_api_str(
        concat!(
            "version: \"0.11.0\"\n",
            "modules:\n",
            "  - name: calculator\n",
            "    errors:\n",
            "      name: CalcError\n",
            "      codes: [{ name: DivisionByZero, code: 1, message: division by zero }]\n",
            "    functions:\n",
            "      - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }\n",
            "      - { name: divide, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32, throws: true }\n",
            "      - { name: greet, params: [{ name: name, type: string }], return: string }\n",
        ),
        "yaml",
    )
    .expect("failed to parse the calculator API")
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
