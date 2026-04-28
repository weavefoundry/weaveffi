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

#[test]
fn extract_with_struct_and_enum() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod shapes {{
    #[weaveffi_struct]
    struct Point {{
        x: f64,
        y: f64,
    }}

    #[weaveffi_enum]
    #[repr(i32)]
    enum Color {{
        Red = 0,
        Green = 1,
        Blue = 2,
    }}

    #[weaveffi_export]
    fn create_point(x: f64, y: f64) -> Point {{
        todo!()
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");

    let module = &api["modules"].as_sequence().unwrap()[0];
    assert_eq!(module["name"].as_str().unwrap(), "shapes");

    let structs = module["structs"].as_sequence().unwrap();
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0]["name"].as_str().unwrap(), "Point");
    let fields = structs[0]["fields"].as_sequence().unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0]["name"].as_str().unwrap(), "x");
    assert_eq!(fields[1]["name"].as_str().unwrap(), "y");

    let enums = module["enums"].as_sequence().unwrap();
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0]["name"].as_str().unwrap(), "Color");
    let variants = enums[0]["variants"].as_sequence().unwrap();
    assert_eq!(variants.len(), 3);
    assert_eq!(variants[0]["name"].as_str().unwrap(), "Red");
    assert_eq!(variants[1]["name"].as_str().unwrap(), "Green");
    assert_eq!(variants[2]["name"].as_str().unwrap(), "Blue");

    let functions = module["functions"].as_sequence().unwrap();
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0]["name"].as_str().unwrap(), "create_point");
}

#[test]
fn extract_with_optional_and_list() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod collections {{
    #[weaveffi_export]
    fn process(items: Vec<i32>, label: Option<String>) -> Option<Vec<i32>> {{
        todo!()
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");

    let func = &api["modules"].as_sequence().unwrap()[0]["functions"]
        .as_sequence()
        .unwrap()[0];
    assert_eq!(func["name"].as_str().unwrap(), "process");

    let params = func["params"].as_sequence().unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0]["name"].as_str().unwrap(), "items");
    assert_eq!(params[0]["type"].as_str().unwrap(), "[i32]");
    assert_eq!(params[1]["name"].as_str().unwrap(), "label");
    assert_eq!(params[1]["type"].as_str().unwrap(), "string?");

    assert_eq!(func["return"].as_str().unwrap(), "[i32]?");
}

#[test]
fn extract_to_json_format() {
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
        .args(["extract", src_path.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to run extract");

    assert!(output.status.success(), "extract --format json failed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let api: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    let modules = api["modules"]
        .as_array()
        .expect("should have modules array");
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["name"].as_str().unwrap(), "math");

    let functions = modules[0]["functions"]
        .as_array()
        .expect("should have functions array");
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0]["name"].as_str().unwrap(), "add");
}

#[test]
fn extract_async_function() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod async_demo {{
    /// Fetch data asynchronously.
    #[weaveffi_export]
    #[weaveffi_async]
    fn fetch(url: String) -> String {{
        todo!()
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let func = &api["modules"].as_sequence().unwrap()[0]["functions"]
        .as_sequence()
        .unwrap()[0];
    assert_eq!(func["name"].as_str().unwrap(), "fetch");
    assert_eq!(
        func["async"].as_bool(),
        Some(true),
        "async flag should be set"
    );
}

#[test]
fn extract_typed_handle_param() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod sessions {{
    #[weaveffi_export]
    fn close(session: *mut Session) {{
        todo!()
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let param = &api["modules"].as_sequence().unwrap()[0]["functions"]
        .as_sequence()
        .unwrap()[0]["params"]
        .as_sequence()
        .unwrap()[0];
    assert_eq!(param["name"].as_str().unwrap(), "session");
    assert_eq!(param["type"].as_str().unwrap(), "handle<Session>");
}

#[test]
fn extract_listener_definition() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod events {{
    /// Fired when data arrives.
    #[weaveffi_callback]
    fn OnData(payload: String) {{}}

    /// Subscribe to OnData events.
    #[weaveffi_listener(event_callback = "OnData")]
    fn data_listener() {{}}
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let module = &api["modules"].as_sequence().unwrap()[0];

    let callbacks = module["callbacks"].as_sequence().unwrap();
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0]["name"].as_str().unwrap(), "OnData");
    assert_eq!(
        callbacks[0]["doc"].as_str(),
        Some("Fired when data arrives.")
    );

    let listeners = module["listeners"].as_sequence().unwrap();
    assert_eq!(listeners.len(), 1);
    assert_eq!(listeners[0]["name"].as_str().unwrap(), "data_listener");
    assert_eq!(
        listeners[0]["event_callback"].as_str().unwrap(),
        "OnData",
        "listener should reference its callback by name"
    );
    assert_eq!(
        listeners[0]["doc"].as_str(),
        Some("Subscribe to OnData events.")
    );
}

#[test]
fn extract_deprecated_attribute_to_since() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod legacy {{
    /// Legacy add.
    #[weaveffi_export]
    #[deprecated(since = "0.2.0", note = "Use add_v2 instead")]
    fn add_old(a: i32, b: i32) -> i32 {{
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let func = &api["modules"].as_sequence().unwrap()[0]["functions"]
        .as_sequence()
        .unwrap()[0];
    assert_eq!(func["name"].as_str().unwrap(), "add_old");
    assert_eq!(func["since"].as_str().unwrap(), "0.2.0");
    assert_eq!(func["deprecated"].as_str().unwrap(), "Use add_v2 instead");
}

#[test]
fn extract_mutable_reference_to_mutable_flag() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");

    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod buffers {{
    #[weaveffi_struct]
    struct Buffer {{
        capacity: i32,
    }}

    #[weaveffi_export]
    fn fill(buf: &mut Buffer, value: i32) {{
        todo!()
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
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    let params = &api["modules"].as_sequence().unwrap()[0]["functions"]
        .as_sequence()
        .unwrap()[0]["params"]
        .as_sequence()
        .unwrap();
    assert_eq!(params[0]["name"].as_str().unwrap(), "buf");
    assert_eq!(params[0]["mutable"].as_bool(), Some(true));
    assert_eq!(params[1]["name"].as_str().unwrap(), "value");
    // value is not &mut, so mutable is false (omitted from YAML by serde
    // default skip).
    assert!(
        params[1].get("mutable").is_none() || params[1]["mutable"].as_bool() == Some(false),
        "non-mut param should not have mutable=true: {:?}",
        params[1]
    );
}

#[test]
fn extract_to_toml_format() {
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
        .args(["extract", src_path.to_str().unwrap(), "--format", "toml"])
        .output()
        .expect("failed to run extract");

    assert!(output.status.success(), "extract --format toml failed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let api: toml::Value = stdout.parse().expect("output should be valid TOML");

    let modules = api["modules"]
        .as_array()
        .expect("should have modules array");
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["name"].as_str().unwrap(), "math");

    let functions = modules[0]["functions"]
        .as_array()
        .expect("should have functions array");
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0]["name"].as_str().unwrap(), "add");
}
