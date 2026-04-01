use std::path::Path;

#[test]
fn generate_dart_contacts() {
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
            "--target",
            "dart",
        ])
        .assert()
        .success();

    let dart = std::fs::read_to_string(out_path.join("dart/lib/weaveffi.dart"))
        .expect("missing dart/lib/weaveffi.dart");
    assert!(
        dart.contains("import 'dart:ffi'"),
        "weaveffi.dart should import dart:ffi"
    );
    assert!(
        dart.contains("enum ContactType {"),
        "weaveffi.dart should contain ContactType enum"
    );
    assert!(
        dart.contains("class Contact {"),
        "weaveffi.dart should contain Contact class"
    );
    assert!(
        dart.contains("weaveffi_contacts_create_contact"),
        "weaveffi.dart should contain create_contact symbol"
    );

    assert!(
        out_path.join("dart/pubspec.yaml").exists(),
        "missing dart/pubspec.yaml"
    );
    assert!(
        out_path.join("dart/README.md").exists(),
        "missing dart/README.md"
    );
}
