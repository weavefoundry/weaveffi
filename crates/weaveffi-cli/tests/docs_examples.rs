const GETTING_STARTED_YAML: &str = r#"
version: "0.1.0"
modules:
  - name: math
    structs:
      - name: Point
        fields:
          - { name: x, type: f64 }
          - { name: y, type: f64 }
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
"#;

#[test]
fn getting_started_yaml_parses_and_validates() {
    let api = weaveffi_ir::parse::parse_api_str(GETTING_STARTED_YAML, "yaml")
        .expect("getting-started YAML should parse");
    weaveffi_core::validate::validate_api(&api).expect("getting-started YAML should validate");

    assert_eq!(api.modules.len(), 1);
    assert_eq!(api.modules[0].name, "math");
    assert_eq!(api.modules[0].structs.len(), 1);
    assert_eq!(api.modules[0].structs[0].name, "Point");
    assert_eq!(api.modules[0].structs[0].fields.len(), 2);
    assert_eq!(api.modules[0].functions.len(), 1);
    assert_eq!(api.modules[0].functions[0].name, "add");
}

#[test]
fn getting_started_yaml_generates_all_targets() {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    let yml_path = out_path.join("weaveffi.yml");
    std::fs::write(&yml_path, GETTING_STARTED_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.join("generated").to_str().unwrap(),
            "--scaffold",
        ])
        .assert()
        .success();

    let gen = out_path.join("generated");
    assert!(gen.join("c/weaveffi.h").exists(), "missing c/weaveffi.h");
    assert!(
        gen.join("swift/Package.swift").exists(),
        "missing swift/Package.swift"
    );
    assert!(
        gen.join("node/types.d.ts").exists(),
        "missing node/types.d.ts"
    );
    assert!(gen.join("scaffold.rs").exists(), "missing scaffold.rs");

    let header = std::fs::read_to_string(gen.join("c/weaveffi.h")).unwrap();
    assert!(
        header.contains("weaveffi_math_add"),
        "C header should contain weaveffi_math_add"
    );
    assert!(
        header.contains("weaveffi_math_Point"),
        "C header should contain weaveffi_math_Point"
    );

    let dts = std::fs::read_to_string(gen.join("node/types.d.ts")).unwrap();
    assert!(
        dts.contains("interface Point"),
        "types.d.ts should contain Point interface"
    );
    assert!(
        dts.contains("function add"),
        "types.d.ts should contain add function"
    );

    let scaffold = std::fs::read_to_string(gen.join("scaffold.rs")).unwrap();
    assert!(
        scaffold.contains("weaveffi_math_add"),
        "scaffold should contain weaveffi_math_add"
    );
    assert!(
        scaffold.contains("weaveffi_math_Point"),
        "scaffold should contain weaveffi_math_Point"
    );
}

#[test]
fn getting_started_yaml_validates_via_cli() {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = out_dir.path().join("weaveffi.yml");
    std::fs::write(&yml_path, GETTING_STARTED_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["validate", yml_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("Validation passed"));
}
