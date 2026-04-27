//! Cross-generator parity test for user-declared callbacks.
//!
//! Builds a minimal API with a callback `OnTick(value: i32)` and a
//! function `register_ticker(callback: callback<OnTick>) -> i32`, generates
//! bindings for all 11 targets, and asserts that each output file contains
//! the callback typedef/typealias/delegate in the idiomatic form for that
//! language. Every target must emit a named type for the callback so users
//! can refer to it in their own code; if any generator drops the type alias
//! this test fails.

const CALLBACKS_YML: &str = "version: \"0.1.0\"
modules:
  - name: ticker
    callbacks:
      - name: OnTick
        params:
          - { name: value, type: i32 }
    functions:
      - name: register_ticker
        params:
          - { name: callback, type: \"callback<OnTick>\" }
        return: i32
";

// Re-enabled once generators advertise the Callbacks capability.
#[ignore = "no generator advertises Callbacks yet; reactivate when later phases add the capability back"]
#[test]
fn callback_typedef_emitted_by_all_generators() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("ticker.yml");
    std::fs::write(&yml_path, CALLBACKS_YML).expect("failed to write ticker.yml");

    let out_path = dir.path().join("out");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let cases: &[(&str, &str, &str)] = &[
        (
            "c",
            "c/weaveffi.h",
            "typedef void (*weaveffi_ticker_OnTick)",
        ),
        ("cpp", "cpp/weaveffi.hpp", "using OnTick = std::function"),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            "typealias OnTick",
        ),
        (
            "android",
            "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            "typealias OnTick",
        ),
        ("node", "node/types.d.ts", "export type OnTick"),
        ("wasm", "wasm/weaveffi_wasm.d.ts", "export type OnTick"),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            "_OnTick = ctypes.CFUNCTYPE",
        ),
        ("dotnet", "dotnet/WeaveFFI.cs", "delegate void OnTick"),
        ("dart", "dart/lib/src/bindings.dart", "typedef OnTick"),
        ("go", "go/weaveffi.go", "type OnTick func"),
        ("ruby", "ruby/lib/weaveffi.rb", "callback :OnTick"),
    ];

    for (target, rel, needle) in cases {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), target));
        assert!(
            content.contains(needle),
            "target {target}: {} should contain {needle:?} for callback OnTick\n--- file ---\n{content}",
            rel
        );
    }
}
