//! Cross-generator bytes-return free discipline audit.
//!
//! Builds a minimal one-function API (`parity.echo(b: bytes) -> bytes`),
//! generates bindings for all 11 targets, and asserts that every target's
//! primary wrapper file contains at least one call to the `weaveffi_free_bytes`
//! helper (or its language-specific named equivalent).
//!
//! Bytes returns are owned `uint8_t*` buffers allocated by the Rust runtime
//! and paired with a `size_t* out_len` out-parameter. Every target wrapper
//! that copies such a buffer back into its native byte-array type must
//! subsequently free the underlying allocation via `weaveffi_free_bytes` —
//! otherwise every bytes-returning call leaks.

const ECHO_YML: &str = "version: \"0.1.0\"
modules:
  - name: parity
    functions:
      - name: echo
        params:
          - { name: b, type: bytes }
        return: bytes
";

#[test]
fn every_generator_frees_returned_bytes() {
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
    //   - bare `weaveffi_free_bytes` for C/C++/Swift/Android JNI/Node addon/Ruby
    //   - `_lib.weaveffi_free_bytes` for Python ctypes
    //   - `_weaveffiFreeBytes` for Dart's typedef binding
    //   - `NativeMethods.weaveffi_free_bytes` for .NET P/Invoke
    //   - `C.weaveffi_free_bytes` for Go cgo
    //   - `wasm.weaveffi_free_bytes` for WASM JS exports
    let targets: &[(&str, &str, &str)] = &[
        ("c", "c/weaveffi.h", "weaveffi_free_bytes"),
        ("cpp", "cpp/weaveffi.hpp", "weaveffi_free_bytes"),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            "weaveffi_free_bytes",
        ),
        (
            "android",
            "android/src/main/cpp/weaveffi_jni.c",
            "weaveffi_free_bytes",
        ),
        ("node", "node/weaveffi_addon.c", "weaveffi_free_bytes"),
        (
            "dotnet",
            "dotnet/WeaveFFI.cs",
            "NativeMethods.weaveffi_free_bytes",
        ),
        ("go", "go/weaveffi.go", "C.weaveffi_free_bytes"),
        ("ruby", "ruby/lib/weaveffi.rb", "weaveffi_free_bytes"),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            "_lib.weaveffi_free_bytes",
        ),
        ("dart", "dart/lib/weaveffi.dart", "_weaveffiFreeBytes"),
        ("wasm", "wasm/weaveffi_wasm.js", "wasm.weaveffi_free_bytes"),
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
             so that byte buffers returned across the C ABI are freed by the caller"
        );
    }
}
