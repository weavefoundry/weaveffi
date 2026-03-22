use std::path::Path;

#[test]
fn generate_contacts_produces_all_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/contacts/contacts.yml");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    assert!(
        header.contains("weaveffi_contacts_create_contact"),
        "c/weaveffi.h should contain weaveffi_contacts_create_contact"
    );

    assert!(
        out_path.join("swift/Package.swift").exists(),
        "missing swift/Package.swift"
    );
    assert!(
        out_path.join("android/build.gradle").exists(),
        "missing android/build.gradle"
    );

    let types_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    assert!(
        types_dts.contains("interface Contact"),
        "node/types.d.ts should contain interface Contact"
    );

    assert!(
        out_path.join("wasm/README.md").exists(),
        "missing wasm/README.md"
    );
}
