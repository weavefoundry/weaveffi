//! Cross-generator struct-handle destroy discipline audit.
//!
//! Builds a minimal one-struct API (`contacts.Contact { id: i64, name: string }`),
//! generates bindings for every supported target, and asserts that each
//! target's primary wrapper file contains at least one reference to the
//! struct's C ABI destroy symbol (`weaveffi_contacts_Contact_destroy`).
//!
//! Struct handles cross the C ABI as owned pointers allocated by the Rust
//! runtime. Every target wrapper that hands a struct handle to user code must
//! route cleanup back through `weaveffi_{module}_{struct}_destroy` —
//! otherwise every struct handle leaks.
//!
//! WASM is intentionally excluded: its current JS wrapper exposes struct
//! handles as opaque integers with no dispose/close surface, so there is no
//! destroy call site to audit yet.
//!
//! This guards the whole cross-target struct-cleanup contract against
//! regressions: if any listed generator silently stops emitting the destroy
//! call, this test fails.

const CONTACT_YML: &str = "version: \"0.1.0\"
modules:
  - name: contacts
    structs:
      - name: Contact
        fields:
          - { name: id, type: i64 }
          - { name: name, type: string }
    functions: []
";

#[test]
fn every_generator_destroys_structs() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("contact.yml");
    std::fs::write(&yml_path, CONTACT_YML).expect("failed to write contact.yml");

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

    let needle = "weaveffi_contacts_Contact_destroy";

    // (target, relative primary wrapper path) for every target that currently
    // emits a struct-handle destroy call site. WASM is deliberately omitted:
    // its JS wrapper keeps struct handles opaque and does not expose a
    // dispose method today.
    let targets: &[(&str, &str)] = &[
        ("c", "c/weaveffi.h"),
        ("cpp", "cpp/weaveffi.hpp"),
        ("swift", "swift/Sources/WeaveFFI/WeaveFFI.swift"),
        ("android", "android/src/main/cpp/weaveffi_jni.c"),
        ("node", "node/weaveffi_addon.c"),
        ("dotnet", "dotnet/WeaveFFI.cs"),
        ("go", "go/weaveffi.go"),
        ("ruby", "ruby/lib/weaveffi.rb"),
        ("python", "python/weaveffi/weaveffi.py"),
        ("dart", "dart/lib/weaveffi.dart"),
    ];

    assert_eq!(
        targets.len(),
        10,
        "audit must cover every target that emits struct-destroy call sites"
    );

    for (name, rel) in targets {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), name));
        assert!(
            content.contains(needle),
            "{name}: expected at least one occurrence of `{needle}` in {rel} \
             so that struct handles returned across the C ABI are destroyed \
             by the caller"
        );
    }
}
