use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_example(rel: &str) -> String {
    std::fs::read_to_string(workspace_root().join(rel)).unwrap_or_else(|err| {
        panic!("failed to read {rel}: {err}");
    })
}

#[test]
fn generate_ruby_contacts() {
    let repo_root = workspace_root();
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

#[test]
fn ruby_contacts_example_files_exist() {
    for file in [
        "examples/ruby/contacts/Gemfile",
        "examples/ruby/contacts/bin/contacts.rb",
        "examples/ruby/contacts/README.md",
    ] {
        assert!(
            workspace_root().join(file).exists(),
            "Ruby contacts example is missing {file}"
        );
    }
}

#[test]
fn ruby_contacts_example_uses_generated_gem_and_crud_api() {
    let gemfile = read_example("examples/ruby/contacts/Gemfile");
    assert!(
        gemfile.contains("gem \"weaveffi\", path: \"../../../generated/ruby\""),
        "Gemfile must use the generated Ruby gem: {gemfile}"
    );

    let script = read_example("examples/ruby/contacts/bin/contacts.rb");
    for token in [
        "require \"ffi\"",
        "require \"weaveffi\"",
        "WeaveFFI.create_contact(",
        "WeaveFFI.list_contacts",
        "WeaveFFI.get_contact(",
        "WeaveFFI.delete_contact(",
        "WeaveFFI::ContactType::PERSONAL",
        "WeaveFFI::ContactType::WORK",
        "FFI::AutoPointer",
        "Contact#destroy",
        "GC.start",
    ] {
        assert!(
            script.contains(token),
            "contacts.rb must mention `{token}`: {script}"
        );
    }
}

#[test]
fn ruby_contacts_readme_documents_build_generate_and_run() {
    let readme = read_example("examples/ruby/contacts/README.md");
    for token in [
        "cargo build -p contacts",
        "cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target ruby",
        "libweaveffi.dylib",
        "libweaveffi.so",
        "bundle install",
        "LD_LIBRARY_PATH=\"$PWD/../../../target/debug\" bundle exec ruby bin/contacts.rb",
        "DYLD_LIBRARY_PATH=\"$PWD/../../../target/debug\" bundle exec ruby bin/contacts.rb",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn ruby_sqlite_contacts_example_files_exist() {
    for file in [
        "examples/ruby/sqlite-contacts/Gemfile",
        "examples/ruby/sqlite-contacts/bin/contacts.rb",
        "examples/ruby/sqlite-contacts/README.md",
    ] {
        assert!(
            workspace_root().join(file).exists(),
            "Ruby SQLite contacts example is missing {file}"
        );
    }
}

#[test]
fn ruby_sqlite_contacts_example_uses_async_blocks_and_enumerator() {
    let gemfile = read_example("examples/ruby/sqlite-contacts/Gemfile");
    assert!(
        gemfile.contains("gem \"weaveffi\", path: \"../../../generated/ruby\""),
        "Gemfile must use the generated Ruby gem: {gemfile}"
    );

    let script = read_example("examples/ruby/sqlite-contacts/bin/contacts.rb");
    for token in [
        "require \"weaveffi\"",
        "Queue.new",
        "WeaveFFI.create_contact_async(",
        "do |result, err|",
        "WeaveFFI.update_contact_async(",
        "WeaveFFI.find_contact_async(",
        "WeaveFFI.count_contacts_async(",
        "WeaveFFI.delete_contact_async(",
        "WeaveFFI.list_contacts(nil)",
        "contacts.each do |contact|",
        "WeaveFFI::Status::ACTIVE",
        "contact.destroy",
    ] {
        assert!(
            script.contains(token),
            "contacts.rb must mention `{token}`: {script}"
        );
    }
    assert!(
        script.contains("contacts.class"),
        "contacts.rb must surface that list_contacts returns an Enumerator: {script}"
    );
}

#[test]
fn ruby_sqlite_contacts_readme_documents_build_generate_and_run() {
    let readme = read_example("examples/ruby/sqlite-contacts/README.md");
    for token in [
        "cargo build -p sqlite-contacts",
        "cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o generated --target ruby",
        "create_contact_async(name, email) { |result, err| ... }",
        "list_contacts(nil)",
        "Enumerator",
        "libweaveffi.dylib",
        "libweaveffi.so",
        "bundle install",
        "LD_LIBRARY_PATH=\"$PWD/../../../target/debug\" bundle exec ruby bin/contacts.rb",
        "DYLD_LIBRARY_PATH=\"$PWD/../../../target/debug\" bundle exec ruby bin/contacts.rb",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}
