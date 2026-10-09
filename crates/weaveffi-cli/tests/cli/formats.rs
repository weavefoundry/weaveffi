//! The JSON spelling of an IDL generates and validates like YAML, and a TOML
//! IDL is rejected (TOML is only for `weaveffi.toml`).

use weaveffi_model::ir::Api;

/// The calculator sample's API, read from YAML.
fn load_calculator_api() -> Api {
    weaveffi_model::parse::parse_api_str(
        concat!(
            "version: \"0.12.0\"\n",
            "modules:\n",
            "  - name: calculator\n",
            "    errors:\n",
            "      - name: CalcError\n",
            "        codes: [{ name: DivisionByZero, code: 1, message: division by zero }]\n",
            "    functions:\n",
            "      - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }\n",
            "      - { name: divide, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32, throws: CalcError }\n",
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

    crate::weaveffi()
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
fn toml_idl_is_rejected() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let toml_path = tmp.path().join("calculator.toml");
    std::fs::write(&toml_path, "version = \"0.12.0\"\nmodules = []\n")
        .expect("failed to write TOML file");

    let output = crate::weaveffi()
        .args(["validate", toml_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = crate::stderr(&output);
    assert!(
        // miette may wrap anywhere in the message, so compare without spaces.
        stderr.replace(' ', "").contains("calculator.toml(.toml)")
            && stderr.replace(' ', "").contains("yml|yaml|json"),
        "{stderr}"
    );
}

#[test]
fn validate_from_json() {
    let api = load_calculator_api();
    let json = serde_json::to_string_pretty(&api).expect("failed to serialize to JSON");

    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let json_path = tmp.path().join("calculator.json");
    std::fs::write(&json_path, &json).expect("failed to write JSON file");

    crate::weaveffi()
        .args(["validate", json_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("Validation passed"));
}
