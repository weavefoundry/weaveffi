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

    let weaveffi_py = std::fs::read_to_string(out_path.join("python/contacts/weaveffi.py"))
        .expect("missing python/contacts/weaveffi.py");
    assert!(
        weaveffi_py.contains("class ContactBook:"),
        "weaveffi.py should contain the ContactBook interface class"
    );
    assert!(
        weaveffi_py.contains("def __init__(self) -> None:"),
        "weaveffi.py should map the `new` constructor to __init__"
    );
    assert!(
        weaveffi_py.contains(
            "def add(self, first_name: str, last_name: str, email: Optional[str], \
             contact_type: \"ContactType\") -> \"Contact\":"
        ),
        "weaveffi.py should contain the snake_case add method"
    );
    assert!(
        weaveffi_py.contains("class ContactsError(WeaveFFIError):"),
        "weaveffi.py should contain the ContactsError domain class"
    );
    assert!(
        weaveffi_py.contains("class NotFound(ContactsError):"),
        "weaveffi.py should contain the per-code NotFound exception"
    );

    let weaveffi_pyi = std::fs::read_to_string(out_path.join("python/contacts/weaveffi.pyi"))
        .expect("missing python/contacts/weaveffi.pyi");
    assert!(
        weaveffi_pyi.contains("class ContactBook:"),
        "weaveffi.pyi should contain the ContactBook stub"
    );
    assert!(
        weaveffi_pyi.contains("def get(self, id: int) -> \"Contact\": ..."),
        "weaveffi.pyi should contain typed method stubs"
    );
    assert!(
        weaveffi_pyi.contains("class NotFound(ContactsError):"),
        "weaveffi.pyi should contain the typed-error stubs"
    );

    assert!(
        out_path.join("python/pyproject.toml").exists(),
        "missing python/pyproject.toml"
    );
}
