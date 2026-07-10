use std::path::Path;

#[test]
fn generate_dotnet_contacts() {
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
            "dotnet",
        ])
        .assert()
        .success();

    let cs = std::fs::read_to_string(out_path.join("dotnet/WeaveFFI.cs"))
        .expect("missing dotnet/WeaveFFI.cs");
    assert!(
        cs.contains("DllImport"),
        "WeaveFFI.cs should contain DllImport"
    );

    // Interface: opaque-handle class with a real ctor, PascalCase methods,
    // camelCase parameters, and Dispose lowering to the destroy symbol.
    assert!(
        cs.contains("public class ContactBook : IDisposable"),
        "WeaveFFI.cs should contain the ContactBook interface class"
    );
    assert!(
        cs.contains("public ContactBook()"),
        "WeaveFFI.cs should map the `new` constructor to a real C# constructor"
    );
    assert!(
        cs.contains(
            "public Contact Add(string firstName, string lastName, string? email, \
             ContactType contactType)"
        ),
        "WeaveFFI.cs should contain the PascalCase Add method with camelCase parameters"
    );
    assert!(
        cs.contains("public Contact Get(long id)"),
        "WeaveFFI.cs should contain the Get method"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_contacts_ContactBook_destroy(_handle);"),
        "WeaveFFI.cs should dispose through the interface destroy symbol"
    );

    // Typed errors: domain exception, code constants, per-domain check.
    assert!(
        cs.contains("public class ContactsException : WeaveFFIException"),
        "WeaveFFI.cs should contain the ContactsException domain exception"
    );
    assert!(
        cs.contains("public const int InvalidName = 1;")
            && cs.contains("public const int NotFound = 2;"),
        "WeaveFFI.cs should surface the domain codes as constants"
    );
    assert!(
        cs.contains("WeaveFFIError.CheckContacts(err);"),
        "throwing wrappers should report through the typed check helper"
    );

    assert!(
        out_path.join("dotnet/WeaveFFI.csproj").exists(),
        "missing dotnet/WeaveFFI.csproj"
    );
}
