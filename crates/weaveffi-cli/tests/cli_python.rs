use std::path::Path;

#[test]
fn generate_python_contacts() {
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
            "python",
        ])
        .assert()
        .success();

    let weaveffi_py = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        weaveffi_py.contains("def create_contact"),
        "weaveffi.py should contain def create_contact"
    );

    let weaveffi_pyi = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.pyi"))
        .expect("missing python/weaveffi/weaveffi.pyi");
    assert!(
        weaveffi_pyi.contains("def create_contact("),
        "weaveffi.pyi should contain create_contact stub"
    );
    assert!(
        weaveffi_pyi.contains("-> int: ..."),
        "weaveffi.pyi should contain type annotations"
    );

    assert!(
        out_path.join("python/pyproject.toml").exists(),
        "missing python/pyproject.toml"
    );
}
