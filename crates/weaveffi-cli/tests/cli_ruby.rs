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

    let rb = std::fs::read_to_string(out_path.join("ruby/lib/contacts.rb"))
        .expect("missing ruby/lib/contacts.rb");
    assert!(
        rb.contains("module WeaveFFI"),
        "contacts.rb should contain module WeaveFFI"
    );
    assert!(
        rb.contains("module ContactType"),
        "contacts.rb should contain ContactType enum"
    );
    assert!(
        rb.contains("class Contact"),
        "contacts.rb should contain Contact class"
    );
    assert!(
        rb.contains("class ContactBook"),
        "contacts.rb should contain ContactBook interface class"
    );
    assert!(
        rb.contains("class ContactBookPtr < FFI::AutoPointer"),
        "contacts.rb should release ContactBook through FFI::AutoPointer"
    );
    assert!(
        rb.contains("weaveffi_contacts_ContactBook_add"),
        "contacts.rb should contain the ContactBook add C symbol"
    );
    assert!(
        rb.contains("class ContactsError < Error"),
        "contacts.rb should contain the typed ContactsError domain"
    );
    assert!(
        rb.contains("class NotFound < ContactsError"),
        "contacts.rb should contain the NotFound code subclass"
    );
    assert!(
        rb.contains("def self.check_contacts_error!(err)"),
        "contacts.rb should contain the typed error checker"
    );

    assert!(
        out_path.join("ruby/contacts.gemspec").exists(),
        "missing ruby/contacts.gemspec"
    );
    assert!(
        out_path.join("ruby/README.md").exists(),
        "missing ruby/README.md"
    );
}

#[test]
fn generate_ruby_inventory() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let input = Path::new(manifest_dir).join("tests/fixtures/03_inventory.yml");

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

    let rb = std::fs::read_to_string(out_path.join("ruby/lib/03_inventory.rb"))
        .expect("missing ruby/lib/03_inventory.rb");
    assert!(
        rb.contains("class Catalog"),
        "inventory bindings should contain Catalog interface class"
    );
    assert!(
        rb.contains("class CatalogPtr < FFI::AutoPointer"),
        "inventory bindings should release Catalog through FFI::AutoPointer"
    );
    assert!(
        rb.contains("class ProductsError < Error"),
        "inventory bindings should contain the ProductsError domain"
    );
    assert!(
        rb.contains("class OrdersError < Error"),
        "inventory bindings should contain the OrdersError domain"
    );
    // Throwing members route through their module's typed checker; the
    // non-throwing `remove` stays on the generic one.
    assert!(
        rb.contains("WeaveFFI.check_products_error!(err)"),
        "Catalog methods should raise typed ProductsError"
    );
    assert!(
        rb.contains("check_orders_error!(err)"),
        "orders functions should raise typed OrdersError"
    );
}
