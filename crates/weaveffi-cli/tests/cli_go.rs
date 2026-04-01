use std::path::Path;

#[test]
fn generate_go_contacts() {
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
            "go",
        ])
        .assert()
        .success();

    let go =
        std::fs::read_to_string(out_path.join("go/weaveffi.go")).expect("missing go/weaveffi.go");
    assert!(
        go.contains("package weaveffi"),
        "weaveffi.go should contain package declaration"
    );
    assert!(
        go.contains("type ContactType int32"),
        "weaveffi.go should contain ContactType enum"
    );
    assert!(
        go.contains("type Contact struct {"),
        "weaveffi.go should contain Contact struct"
    );
    assert!(
        go.contains("weaveffi_contacts_create_contact"),
        "weaveffi.go should contain create_contact C symbol"
    );

    assert!(out_path.join("go/go.mod").exists(), "missing go/go.mod");
    assert!(
        out_path.join("go/README.md").exists(),
        "missing go/README.md"
    );
}
