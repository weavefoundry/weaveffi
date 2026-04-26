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

    let bindings = std::fs::read_to_string(out_path.join("dart/lib/src/bindings.dart"))
        .expect("missing dart/lib/src/bindings.dart");
    assert!(
        bindings.contains("import 'dart:ffi'"),
        "bindings.dart should import dart:ffi"
    );
    assert!(
        bindings.contains("enum ContactType {"),
        "bindings.dart should contain ContactType enum"
    );
    assert!(
        bindings.contains("class Contact {"),
        "bindings.dart should contain Contact class"
    );
    assert!(
        bindings.contains("weaveffi_contacts_create_contact"),
        "bindings.dart should contain create_contact symbol"
    );

    let barrel = std::fs::read_to_string(out_path.join("dart/lib/weaveffi.dart"))
        .expect("missing dart/lib/weaveffi.dart");
    assert!(
        barrel.contains("export 'src/bindings.dart';"),
        "weaveffi.dart should re-export the internal bindings: {barrel}"
    );

    assert!(
        out_path.join("dart/pubspec.yaml").exists(),
        "missing dart/pubspec.yaml"
    );
    assert!(
        out_path.join("dart/analysis_options.yaml").exists(),
        "missing dart/analysis_options.yaml"
    );
    assert!(
        out_path.join("dart/README.md").exists(),
        "missing dart/README.md"
    );
}
