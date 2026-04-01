use std::path::Path;

#[test]
fn generate_ruby_contacts() {
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
            "ruby",
        ])
        .assert()
        .success();

    let rb = std::fs::read_to_string(out_path.join("ruby/lib/weaveffi.rb"))
        .expect("missing ruby/lib/weaveffi.rb");
    assert!(
        rb.contains("module WeaveFFI"),
        "weaveffi.rb should contain module WeaveFFI"
    );
    assert!(
        rb.contains("module ContactType"),
        "weaveffi.rb should contain ContactType enum"
    );
    assert!(
        rb.contains("class Contact"),
        "weaveffi.rb should contain Contact class"
    );
    assert!(
        rb.contains("weaveffi_contacts_create_contact"),
        "weaveffi.rb should contain create_contact C symbol"
    );

    assert!(
        out_path.join("ruby/weaveffi.gemspec").exists(),
        "missing ruby/weaveffi.gemspec"
    );
    assert!(
        out_path.join("ruby/README.md").exists(),
        "missing ruby/README.md"
    );
}
