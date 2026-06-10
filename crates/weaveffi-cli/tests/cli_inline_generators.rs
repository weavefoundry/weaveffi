use std::fs;

#[test]
fn inline_dart_package_name_used() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(
        &yml,
        concat!(
            "version: \"0.3.0\"\n",
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
    "version: \"0.3.0\"\n",
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

/// The wasm target declares callbacks/listeners unsupported, so generating a
/// listener-bearing IDL for wasm must fail loudly with an actionable message.
#[test]
fn wasm_rejects_listeners_without_opt_in() {
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
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("target 'wasm' does not support"),
        "stderr should name the failing target:\n{stderr}"
    );
    assert!(
        stderr.contains("events.message_listener"),
        "stderr should list the offending declaration:\n{stderr}"
    );
    assert!(
        stderr.contains("allow_unsupported"),
        "stderr should mention the opt-in escape hatch:\n{stderr}"
    );
    assert!(!out.join("wasm").exists(), "no output should be written");
}

/// `generators.wasm.allow_unsupported: true` downgrades the capability
/// failure to a warning; the supported surface generates and the listener
/// becomes an explicit throwing stub.
#[test]
fn wasm_allow_unsupported_generates_with_warning() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(
        &yml,
        format!("{LISTENER_IDL}generators:\n  wasm:\n    allow_unsupported: true\n"),
    )
    .expect("failed to write api.yml");
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
        stderr.contains("warning: target 'wasm'"),
        "opting in must still print a warning:\n{stderr}"
    );
    assert!(
        stderr.contains("events.message_listener"),
        "warning should list what was skipped:\n{stderr}"
    );

    let js = fs::read_to_string(out.join("wasm/weaveffi_wasm.js"))
        .expect("missing wasm/weaveffi_wasm.js");
    assert!(
        js.contains("register_message_listener() {"),
        "listener should become an explicit stub:\n{js}"
    );
    assert!(
        js.contains("is not supported by the wasm target"),
        "stub should throw with a clear message:\n{js}"
    );

    let dts = fs::read_to_string(out.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        !dts.contains("register_message_listener"),
        "d.ts should omit unsupported entry points:\n{dts}"
    );
}
