//! End-to-end tests for `weaveffi extract`, which reads annotated Rust source
//! (via the shared `weaveffi-bridge`) and emits the IDL. The annotation scheme
//! is the one the `#[weaveffi::module]` macro uses: a `#[weaveffi::module]`
//! marks the exported namespace, and item markers (`#[weaveffi::export]`,
//! `#[weaveffi::record]`, `#[weaveffi::enumeration]`, ...) tag the surface.

use std::io::Write;

fn write_src(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");
    let mut f = std::fs::File::create(&src_path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    (dir, src_path)
}

#[test]
fn extract_basic_rust_file() {
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod math {
    #[weaveffi::export]
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
"#,
    );

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run extract");

    assert!(output.status.success(), "extract command failed");

    let stdout = String::from_utf8(output.stdout).unwrap();
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
fn unmarked_module_is_ignored() {
    // A module without #[weaveffi::module] exports nothing, even if its items
    // carry item markers: the module attribute is the opt-in.
    let (_dir, src_path) = write_src(
        r#"
mod plain {
    #[weaveffi::export]
    fn add(a: i32) -> i32 { a }
}
"#,
    );

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run extract");

    assert!(output.status.success(), "extract command failed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let api: serde_yaml::Value =
        serde_yaml::from_str(&stdout).expect("output should be valid YAML");
    assert!(
        api["modules"]
            .as_sequence()
            .map(|m| m.is_empty())
            .unwrap_or(true),
        "an unmarked module should produce no exported modules: {stdout}"
    );
}

#[test]
fn extract_with_struct_and_enum() {
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod shapes {
    #[weaveffi::record]
    struct Point {
        x: f64,
        y: f64,
    }

    #[weaveffi::enumeration]
    #[repr(i32)]
    enum Color {
        Red = 0,
        Green = 1,
        Blue = 2,
    }

    #[weaveffi::export]
    fn create_point(x: f64, y: f64) -> Point {
        todo!()
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod collections {
    #[weaveffi::export]
    fn process(items: Vec<i32>, label: Option<String>) -> Option<Vec<i32>> {
        todo!()
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod math {
    #[weaveffi::export]
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod async_demo {
    /// Fetch data asynchronously.
    #[weaveffi::export]
    async fn fetch(url: String) -> String {
        todo!()
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod sessions {
    #[weaveffi::export]
    fn close(session: *mut Session) {
        todo!()
    }
}
"#,
    );

    // `Session` is an opaque handle target the source never declares, so the
    // extracted IDL does not validate; `--warn` emits it anyway for bootstrapping.
    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap(), "--warn"])
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
fn extract_fails_loud_on_invalid_api_without_warn() {
    // An undeclared handle target makes the extracted API fail validation.
    // Without `--warn`, `extract` must abort instead of emitting broken IDL.
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod sessions {
    #[weaveffi::export]
    fn close(session: *mut Session) {
        todo!()
    }
}
"#,
    );

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run extract");

    assert!(
        !output.status.success(),
        "extract should fail loudly on an API that does not validate"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--warn"),
        "the error should point users at --warn: {stderr}"
    );
}

#[test]
fn extract_listener_definition() {
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod events {
    /// Fired when data arrives.
    #[weaveffi::callback]
    fn OnData(payload: String) {}

    /// Subscribe to OnData events.
    #[weaveffi::listener(event = "OnData")]
    fn data_listener() {}
}
"#,
    );

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
fn extract_interface_with_error_domain() {
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod store {
    #[weaveffi::error]
    enum StoreError {
        /// resource is missing
        Missing = 1,
    }

    /// A tiny stateful object.
    #[weaveffi::interface]
    struct Session {
        id: i64,
    }

    impl Session {
        /// Open a session, failing when the id is invalid.
        pub fn open(id: i64) -> Result<Session, StoreError> {
            Ok(Session { id })
        }

        /// Fetch a value, failing when it is missing.
        pub fn fetch(&self, key: String) -> Result<i64, StoreError> {
            Err(StoreError::Missing)
        }

        /// The session subsystem version.
        pub fn version() -> i32 {
            1
        }
    }
}
"#,
    );

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

    let errors = &module["errors"];
    assert_eq!(errors["name"].as_str().unwrap(), "StoreError");
    let codes = errors["codes"].as_sequence().unwrap();
    assert_eq!(codes.len(), 1);
    assert_eq!(codes[0]["name"].as_str().unwrap(), "Missing");
    assert_eq!(codes[0]["code"].as_i64(), Some(1));
    assert_eq!(
        codes[0]["message"].as_str().unwrap(),
        "resource is missing",
        "the variant's doc comment becomes the default message"
    );

    let interfaces = module["interfaces"].as_sequence().unwrap();
    assert_eq!(interfaces.len(), 1);
    let session = &interfaces[0];
    assert_eq!(session["name"].as_str().unwrap(), "Session");
    assert_eq!(session["doc"].as_str(), Some("A tiny stateful object."));

    let constructors = session["constructors"].as_sequence().unwrap();
    assert_eq!(constructors.len(), 1);
    assert_eq!(constructors[0]["name"].as_str().unwrap(), "open");
    assert_eq!(
        constructors[0]["throws"].as_bool(),
        Some(true),
        "a Result<Self, E> constructor should extract as throws: true"
    );

    let methods = session["methods"].as_sequence().unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0]["name"].as_str().unwrap(), "fetch");
    assert_eq!(methods[0]["throws"].as_bool(), Some(true));
    assert_eq!(
        methods[0]["params"].as_sequence().unwrap()[0]["type"]
            .as_str()
            .unwrap(),
        "string"
    );
    assert_eq!(methods[0]["return"].as_str().unwrap(), "i64");

    let statics = session["statics"].as_sequence().unwrap();
    assert_eq!(statics.len(), 1);
    assert_eq!(statics[0]["name"].as_str().unwrap(), "version");
    assert_eq!(statics[0]["return"].as_str().unwrap(), "i32");
}

#[test]
fn extract_deprecated_attribute_to_since() {
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod legacy {
    /// Legacy add.
    #[weaveffi::export]
    #[deprecated(since = "0.2.0", note = "Use add_v2 instead")]
    fn add_old(a: i32, b: i32) -> i32 {
        a + b
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod buffers {
    #[weaveffi::record]
    struct Buffer {
        capacity: i32,
    }

    #[weaveffi::export]
    fn fill(buf: &mut Buffer, value: i32) {
        todo!()
    }
}
"#,
    );

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
    let (_dir, src_path) = write_src(
        r#"
#[weaveffi::module]
mod math {
    #[weaveffi::export]
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
"#,
    );

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
