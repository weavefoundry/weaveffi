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

    // Typed error surface: one domain exception per declaring module plus a
    // subclass per code, and a domain check helper used by throwing wrappers.
    assert!(
        hpp.contains("class WeaveFFIError : public std::runtime_error"),
        "missing generic exception"
    );
    assert!(
        hpp.contains("class ContactsError : public WeaveFFIError"),
        "missing domain exception"
    );
    assert!(
        hpp.contains("class NotFoundError : public ContactsError"),
        "missing per-code exception subclass"
    );
    assert!(
        hpp.contains("inline void check_contacts(weaveffi_error& err)"),
        "missing per-domain check helper"
    );

    // Interface: RAII class with the canonical constructor mapped from `new`,
    // methods on the wrapped handle, and the destroy symbol in the destructor.
    assert!(
        hpp.contains("class ContactBook {"),
        "missing interface class"
    );
    assert!(
        hpp.contains("weaveffi_contacts_ContactBook_destroy"),
        "interface destructor must call the destroy symbol"
    );
    assert!(
        hpp.contains("detail::check_contacts(err);"),
        "throwing methods must use the domain check helper"
    );

    assert!(
        out_path.join("cpp/CMakeLists.txt").exists(),
        "missing cpp/CMakeLists.txt"
    );
}
