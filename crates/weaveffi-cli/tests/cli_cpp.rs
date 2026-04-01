use std::path::Path;

#[test]
fn generate_cpp_contacts() {
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
            "cpp",
        ])
        .assert()
        .success();

    let hpp = std::fs::read_to_string(out_path.join("cpp/weaveffi.hpp"))
        .expect("missing cpp/weaveffi.hpp");
    assert!(
        hpp.contains("namespace weaveffi"),
        "weaveffi.hpp should contain namespace weaveffi"
    );

    assert!(
        out_path.join("cpp/CMakeLists.txt").exists(),
        "missing cpp/CMakeLists.txt"
    );
}
