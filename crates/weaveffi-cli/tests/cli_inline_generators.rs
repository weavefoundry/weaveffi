use std::fs;

#[test]
fn inline_dart_package_name_used() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(
        &yml,
        concat!(
            "version: \"0.5.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "generators:\n",
            "  dart:\n",
            "    package_name: my_inline_dart_pkg\n",
        ),
    )
    .expect("failed to write api.yml");

    let out = dir.path().join("out");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "dart",
        ])
        .assert()
        .success();

    let pubspec =
        fs::read_to_string(out.join("dart/pubspec.yaml")).expect("missing dart/pubspec.yaml");
    assert!(
        pubspec.contains("name: my_inline_dart_pkg"),
        "pubspec should pick up inline dart.package_name override; got:\n{pubspec}"
    );
}

const LISTENER_IDL: &str = concat!(
    "version: \"0.5.0\"\n",
    "modules:\n",
    "  - name: events\n",
    "    callbacks:\n",
    "      - name: OnMessage\n",
    "        params:\n",
    "          - { name: message, type: string }\n",
    "    listeners:\n",
    "      - name: message_listener\n",
    "        event_callback: OnMessage\n",
    "    functions:\n",
    "      - name: send\n",
    "        params:\n",
    "          - { name: text, type: string }\n",
);

/// The wasm target supports listeners in its standard loader: a
/// listener-bearing IDL generates real register/unregister bindings (no
/// opt-in flag, no capability warning, no throwing stubs).
#[test]
fn wasm_generates_listener_bindings() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(&yml, LISTENER_IDL).expect("failed to write api.yml");
    let out = dir.path().join("out");

    let assert = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "wasm",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        !stderr.contains("does not support"),
        "no capability warning should be printed:\n{stderr}"
    );

    let js = fs::read_to_string(out.join("wasm/weaveffi_wasm.js"))
        .expect("missing wasm/weaveffi_wasm.js");
    assert!(
        js.contains("registerMessageListener(callback) {"),
        "register should take the JS callback:\n{js}"
    );
    assert!(
        js.contains("wasm.weaveffi_events_register_message_listener("),
        "register should call the producer symbol:\n{js}"
    );
    assert!(
        !js.contains("is not supported by the wasm target"),
        "no throwing stubs in the standard loader:\n{js}"
    );

    let dts = fs::read_to_string(out.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        dts.contains("registerMessageListener(callback: (message: string) => void): number;"),
        "d.ts should declare the register entry point:\n{dts}"
    );
    assert!(
        dts.contains("unregisterMessageListener(id: number): void;"),
        "d.ts should declare the unregister entry point:\n{dts}"
    );
}

/// `generators.wasm.emscripten: true` swaps the fetch-based loader for one
/// that accepts a pre-initialized Emscripten module and binds its
/// underscore-prefixed exports to the symbol names the glue calls.
#[test]
fn wasm_emscripten_mode_generates_module_loader() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(
        &yml,
        concat!(
            "version: \"0.5.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "generators:\n",
            "  wasm:\n",
            "    emscripten: true\n",
        ),
    )
    .expect("failed to write api.yml");
    let out = dir.path().join("out");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "wasm",
        ])
        .assert()
        .success();

    let js = fs::read_to_string(out.join("wasm/weaveffi_wasm.js"))
        .expect("missing wasm/weaveffi_wasm.js");
    assert!(
        js.contains("export async function loadWeaveffiWasm(module) {"),
        "loader should accept the Emscripten module:\n{js}"
    );
    assert!(
        js.contains("weaveffi_math_add: m['_weaveffi_math_add'],"),
        "loader should bind the underscore-prefixed export:\n{js}"
    );
    assert!(
        !js.contains("WebAssembly.instantiate"),
        "Emscripten mode must not instantiate the wasm itself:\n{js}"
    );

    let dts = fs::read_to_string(out.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        dts.contains("loadWeaveffiWasm(module: object | Promise<object>)"),
        "d.ts should declare the module-taking loader:\n{dts}"
    );
}
