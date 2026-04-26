//! Cross-generator string-return free discipline audit.
//!
//! Builds a minimal one-function API (`parity.echo(s: string) -> string`),
//! generates bindings for all 11 targets, and asserts that every target's
//! primary wrapper file contains at least one call to the `weaveffi_free_string`
//! helper (or its language-specific named equivalent).
//!
//! String returns are owned `const char*` pointers allocated by the Rust
//! runtime. Every target wrapper that reads such a pointer back into its
//! native string type must subsequently free the underlying buffer via
//! `weaveffi_free_string` — otherwise every string-returning call leaks.

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
fn every_generator_frees_returned_strings() {
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

    // (target, relative wrapper path, expected free-call needle).
    // Six distinct call-site patterns cover all 11 targets:
    //   - bare `weaveffi_free_string` for C/C++/Swift/Android JNI/Node addon/Ruby
    //   - `_lib.weaveffi_free_string` for Python ctypes
    //   - `_weaveffiFreeString` for Dart's typedef binding
    //   - `NativeMethods.weaveffi_free_string` for .NET P/Invoke
    //   - `C.weaveffi_free_string` for Go cgo
    //   - `wasm.weaveffi_free_string` for WASM JS exports
    let targets: &[(&str, &str, &str)] = &[
        ("c", "c/weaveffi.h", "weaveffi_free_string"),
        ("cpp", "cpp/weaveffi.hpp", "weaveffi_free_string"),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            "weaveffi_free_string",
        ),
        (
            "android",
            "android/src/main/cpp/weaveffi_jni.c",
            "weaveffi_free_string",
        ),
        ("node", "node/weaveffi_addon.c", "weaveffi_free_string"),
        (
            "dotnet",
            "dotnet/WeaveFFI.cs",
            "NativeMethods.weaveffi_free_string",
        ),
        ("go", "go/weaveffi.go", "C.weaveffi_free_string"),
        ("ruby", "ruby/lib/weaveffi.rb", "weaveffi_free_string"),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            "_lib.weaveffi_free_string",
        ),
        ("dart", "dart/lib/src/bindings.dart", "_weaveffiFreeString"),
        ("wasm", "wasm/weaveffi_wasm.js", "wasm.weaveffi_free_string"),
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
             so that strings returned across the C ABI are freed by the caller"
        );
    }
}
