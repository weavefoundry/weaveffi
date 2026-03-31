use std::io::Write;

#[test]
fn extract_basic_rust_file() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod math {{
    #[weaveffi_export]
    fn add(a: i32, b: i32) -> i32 {{
        a + b
    }}
}}
"#
        )
        .unwrap();
    }

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run extract");

    assert!(output.status.success(), "extract command failed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(
        stdout.contains("math"),
        "output should contain module name 'math': {stdout}"
    );
    assert!(
        stdout.contains("add"),
        "output should contain function name 'add': {stdout}"
    );
    assert!(
        stdout.contains("i32"),
        "output should contain type 'i32': {stdout}"
    );

    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let modules = api["modules"]
        .as_sequence()
        .expect("should have modules array");
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module["name"].as_str().unwrap(), "math");

    let functions = module["functions"]
        .as_sequence()
        .expect("should have functions array");
    assert_eq!(functions.len(), 1);

    let func = &functions[0];
    assert_eq!(func["name"].as_str().unwrap(), "add");

    let params = func["params"]
        .as_sequence()
        .expect("should have params array");
    assert_eq!(params.len(), 2);
    assert_eq!(params[0]["name"].as_str().unwrap(), "a");
    assert_eq!(params[0]["type"].as_str().unwrap(), "i32");
    assert_eq!(params[1]["name"].as_str().unwrap(), "b");
    assert_eq!(params[1]["type"].as_str().unwrap(), "i32");

    assert_eq!(func["return"].as_str().unwrap(), "i32");
}
