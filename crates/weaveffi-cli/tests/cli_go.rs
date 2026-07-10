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
    // The Go package clause follows the resolved package identity (contacts.yml
    // declares `package.name: contacts`), not the `weaveffi` brand.
    assert!(
        go.contains("package contacts"),
        "weaveffi.go should contain identity-derived package declaration"
    );
    assert!(
        go.contains("type ContactType int32"),
        "weaveffi.go should contain ContactType enum"
    );
    assert!(
        go.contains("type Contact struct {"),
        "weaveffi.go should contain Contact struct"
    );

    // The ContactBook interface surfaces as a wrapper struct with a factory
    // constructor, methods on the wrapper, and an explicit Close.
    assert!(
        go.contains("type ContactBook struct {"),
        "weaveffi.go should contain the ContactBook wrapper"
    );
    assert!(
        go.contains("func NewContactBook() *ContactBook {"),
        "ctor named `new` should surface as NewContactBook"
    );
    assert!(
        go.contains("func (s *ContactBook) Add(firstName string, lastName string, email *string, contactType ContactType) (*Contact, error) {"),
        "throwing method should keep the (T, error) shape"
    );
    assert!(
        go.contains("func (s *ContactBook) Count() int32 {"),
        "plain method should have a bare return"
    );
    assert!(
        go.contains("C.weaveffi_contacts_ContactBook_add(s.ptr, "),
        "methods should pass s.ptr as the leading C argument"
    );
    assert!(
        go.contains("C.weaveffi_contacts_ContactBook_destroy(s.ptr)"),
        "Close should call the interface destroy symbol"
    );

    // The typed error domain: one Go error type plus exported code constants.
    assert!(
        go.contains("type ContactsError struct {"),
        "weaveffi.go should contain the typed ContactsError"
    );
    assert!(
        go.contains("ContactsErrorInvalidName int32 = 1"),
        "missing InvalidName code constant"
    );
    assert!(
        go.contains("ContactsErrorNotFound int32 = 2"),
        "missing NotFound code constant"
    );
    assert!(
        go.contains("return nil, wvMapContacts(wvTakeError(&cErr))"),
        "throwing methods should map through the domain helper"
    );

    assert!(out_path.join("go/go.mod").exists(), "missing go/go.mod");
    assert!(
        out_path.join("go/README.md").exists(),
        "missing go/README.md"
    );
}
