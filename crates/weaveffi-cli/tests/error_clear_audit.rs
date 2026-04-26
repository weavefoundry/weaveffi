//! Cross-generator `weaveffi_error_clear` discipline audit.
//!
//! Builds a minimal one-function API (`parity.echo(s: string) -> string`),
//! generates bindings for all 11 targets, and asserts that every target's
//! primary wrapper file contains at least one call to the `weaveffi_error_clear`
//! helper (or its language-specific named equivalent).
//!
//! The `weaveffi_error` struct owns a heap-allocated message pointer. Every
//! wrapper that reads `err.message` back into a native string and then throws
//! or returns must call `weaveffi_error_clear` AFTER capturing the message so
//! the allocation is released — otherwise every failing call leaks the
//! message. Per-generator unit tests already assert the ordering; this audit
//! is the end-to-end safety net that exercises the real CLI pipeline and
//! guards against a generator silently dropping the clear call.

const ECHO_YML: &str = "version: \"0.1.0\"
modules:
  - name: parity
    functions:
      - name: echo
        params:
          - { name: s, type: string }
        return: string
";

#[test]
fn every_generator_clears_weaveffi_error() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("echo.yml");
    std::fs::write(&yml_path, ECHO_YML).expect("failed to write echo.yml");

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

    // (target, relative wrapper path, expected clear-call needle).
    // Six distinct call-site patterns cover all 11 targets:
    //   - bare `weaveffi_error_clear` for C (declaration) and C++/Swift/
    //     Android JNI/Node addon/Ruby (direct C-linkage calls)
    //   - `_lib.weaveffi_error_clear` for Python ctypes
    //   - `_weaveffiErrorClear` for Dart's typedef binding
    //   - `NativeMethods.weaveffi_error_clear` for .NET P/Invoke
    //   - `C.weaveffi_error_clear` for Go cgo
    //   - `wasm.weaveffi_error_clear` for WASM JS exports
    let targets: &[(&str, &str, &str)] = &[
        ("c", "c/weaveffi.h", "weaveffi_error_clear"),
        ("cpp", "cpp/weaveffi.hpp", "weaveffi_error_clear"),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            "weaveffi_error_clear",
        ),
        (
            "android",
            "android/src/main/cpp/weaveffi_jni.c",
            "weaveffi_error_clear",
        ),
        ("node", "node/weaveffi_addon.c", "weaveffi_error_clear"),
        (
            "dotnet",
            "dotnet/WeaveFFI.cs",
            "NativeMethods.weaveffi_error_clear",
        ),
        ("go", "go/weaveffi.go", "C.weaveffi_error_clear"),
        ("ruby", "ruby/lib/weaveffi.rb", "weaveffi_error_clear"),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            "_lib.weaveffi_error_clear",
        ),
        ("dart", "dart/lib/weaveffi.dart", "_weaveffiErrorClear"),
        ("wasm", "wasm/weaveffi_wasm.js", "wasm.weaveffi_error_clear"),
    ];

    assert_eq!(
        targets.len(),
        11,
        "audit must cover all 11 supported targets"
    );

    for (name, rel, needle) in targets {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), name));
        assert!(
            content.contains(needle),
            "{name}: expected at least one occurrence of `{needle}` in {rel} \
             so that weaveffi_error messages captured across the C ABI are \
             released after the caller reads them"
        );
    }
}
