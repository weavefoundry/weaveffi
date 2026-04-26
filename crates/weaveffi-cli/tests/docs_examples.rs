const GENERATOR_DOCS_YAML: &str = r#"
version: "0.1.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }

    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: last_name, type: string }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: "i32?" }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32
"#;

fn generate_all_for_docs() -> (tempfile::TempDir, std::path::PathBuf) {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path().to_path_buf();
    let yml_path = out_path.join("weaveffi.yml");
    std::fs::write(&yml_path, GENERATOR_DOCS_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.join("generated").to_str().unwrap(),
        ])
        .assert()
        .success();

    (out_dir, out_path)
}

#[test]
fn doc_swift_contains_enum_declaration() {
    let (_dir, out) = generate_all_for_docs();
    let swift =
        std::fs::read_to_string(out.join("generated/swift/Sources/WeaveFFI/WeaveFFI.swift"))
            .unwrap();

    assert!(
        swift.contains("public enum ContactType: Int32 {"),
        "Swift enum declaration missing: {swift}"
    );
    assert!(
        swift.contains("case personal = 0"),
        "Swift enum variant lowerCamelCase missing: {swift}"
    );
}

#[test]
fn doc_swift_contains_struct_class() {
    let (_dir, out) = generate_all_for_docs();
    let swift =
        std::fs::read_to_string(out.join("generated/swift/Sources/WeaveFFI/WeaveFFI.swift"))
            .unwrap();

    assert!(
        swift.contains("public class Contact {"),
        "Swift struct class missing: {swift}"
    );
    assert!(
        swift.contains("let ptr: OpaquePointer"),
        "OpaquePointer property missing: {swift}"
    );
    assert!(
        swift.contains("weaveffi_contacts_Contact_destroy(ptr)"),
        "deinit destroy missing: {swift}"
    );
    assert!(
        swift.contains("public var name: String {"),
        "name getter missing: {swift}"
    );
    assert!(
        swift.contains("public var age: Int32 {"),
        "age getter missing: {swift}"
    );
}

#[test]
fn doc_swift_optional_and_list_returns() {
    let (_dir, out) = generate_all_for_docs();
    let swift =
        std::fs::read_to_string(out.join("generated/swift/Sources/WeaveFFI/WeaveFFI.swift"))
            .unwrap();

    assert!(
        swift.contains("-> Contact? {"),
        "optional return missing: {swift}"
    );
    assert!(
        swift.contains("-> [Contact] {"),
        "list return missing: {swift}"
    );
}

#[test]
fn doc_c_header_opaque_struct_and_enum() {
    let (_dir, out) = generate_all_for_docs();
    let header = std::fs::read_to_string(out.join("generated/c/weaveffi.h")).unwrap();

    assert!(
        header.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
        "opaque struct typedef missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_ContactType_Personal = 0"),
        "enum variant missing: {header}"
    );
    assert!(
        header.contains("void weaveffi_contacts_Contact_destroy("),
        "destroy prototype missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_Contact_get_name("),
        "getter missing: {header}"
    );
}

#[test]
fn doc_c_header_naming_convention() {
    let (_dir, out) = generate_all_for_docs();
    let header = std::fs::read_to_string(out.join("generated/c/weaveffi.h")).unwrap();

    assert!(
        header.contains("weaveffi_contacts_create_contact("),
        "function naming convention wrong: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_list_contacts("),
        "list_contacts missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_count_contacts("),
        "count_contacts missing: {header}"
    );
}

#[test]
fn doc_node_dts_interface_and_enum() {
    let (_dir, out) = generate_all_for_docs();
    let dts = std::fs::read_to_string(out.join("generated/node/types.d.ts")).unwrap();

    assert!(
        dts.contains("export declare class Contact {"),
        "TS class missing: {dts}"
    );
    assert!(
        dts.contains("readonly name: string;"),
        "name field missing: {dts}"
    );
    assert!(
        dts.contains("readonly email: string | null;"),
        "optional field missing: {dts}"
    );
    assert!(
        dts.contains("dispose(): void;"),
        "dispose method missing: {dts}"
    );
    assert!(
        dts.contains("export enum ContactType {"),
        "TS enum missing: {dts}"
    );
    assert!(
        dts.contains("  Personal = 0,"),
        "enum variant missing: {dts}"
    );
}

#[test]
fn doc_node_dts_optional_and_list_return() {
    let (_dir, out) = generate_all_for_docs();
    let dts = std::fs::read_to_string(out.join("generated/node/types.d.ts")).unwrap();

    assert!(
        dts.contains("Contact | null"),
        "optional return missing: {dts}"
    );
    assert!(dts.contains("Contact[]"), "list return missing: {dts}");
}

#[test]
fn doc_android_kotlin_wrapper() {
    let (_dir, out) = generate_all_for_docs();
    let kt = std::fs::read_to_string(
        out.join("generated/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"),
    )
    .unwrap();

    assert!(
        kt.contains("System.loadLibrary(\"weaveffi\")"),
        "loadLibrary missing: {kt}"
    );
    assert!(
        kt.contains("@JvmStatic external fun"),
        "external fun missing: {kt}"
    );
    assert!(
        kt.contains("enum class ContactType(val value: Int) {"),
        "enum class missing: {kt}"
    );
    assert!(
        kt.contains("class Contact internal constructor(private var handle: Long)"),
        "struct class missing: {kt}"
    );
}

#[test]
fn doc_android_jni_shim() {
    let (_dir, out) = generate_all_for_docs();
    let jni =
        std::fs::read_to_string(out.join("generated/android/src/main/cpp/weaveffi_jni.c")).unwrap();

    assert!(
        jni.contains("#include \"weaveffi.h\""),
        "missing weaveffi.h include: {jni}"
    );
    assert!(
        jni.contains("JNIEXPORT"),
        "missing JNIEXPORT declarations: {jni}"
    );
    assert!(
        jni.contains("weaveffi_error err = {0, NULL};"),
        "missing error init: {jni}"
    );
}

#[test]
fn doc_android_cmake_exists() {
    let (_dir, out) = generate_all_for_docs();
    let cmake =
        std::fs::read_to_string(out.join("generated/android/src/main/cpp/CMakeLists.txt")).unwrap();

    assert!(cmake.contains("add_library(weaveffi SHARED weaveffi_jni.c)"));
    assert!(cmake.contains("target_include_directories(weaveffi PRIVATE ../../../../c)"));
}

#[test]
fn doc_wasm_loader_generated() {
    let (_dir, out) = generate_all_for_docs();
    let js = std::fs::read_to_string(out.join("generated/wasm/weaveffi_wasm.js")).unwrap();

    assert!(
        js.contains("export async function loadWeaveffiWasm(url)"),
        "loader function missing: {js}"
    );
    assert!(
        js.contains("WebAssembly.instantiate"),
        "instantiate call missing: {js}"
    );
}

#[test]
fn doc_wasm_readme_type_conventions() {
    let (_dir, out) = generate_all_for_docs();
    let readme = std::fs::read_to_string(out.join("generated/wasm/README.md")).unwrap();

    assert!(readme.contains("### Structs"), "structs section missing");
    assert!(readme.contains("### Enums"), "enums section missing");
    assert!(
        readme.contains("### Optionals"),
        "optionals section missing"
    );
    assert!(readme.contains("### Lists"), "lists section missing");
}

#[test]
fn memory_guide_documents_allocator_contract() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let guide = std::fs::read_to_string(workspace_root.join("docs/src/guides/memory.md"))
        .expect("docs/src/guides/memory.md must exist");

    assert!(
        guide.contains("## Allocator contract"),
        "memory guide must have an Allocator contract section: {guide}"
    );
    assert!(
        guide.contains("uint8_t* weaveffi_alloc(size_t size);"),
        "memory guide must document the weaveffi_alloc C signature: {guide}"
    );
    assert!(
        guide.contains("void weaveffi_free(uint8_t* ptr, size_t size);"),
        "memory guide must document the weaveffi_free C signature: {guide}"
    );
    assert!(
        guide.contains("weaveffi_free_string") && guide.contains("weaveffi_free_bytes"),
        "memory guide must mention the typed free helpers alongside the raw allocator: {guide}"
    );
    assert!(
        guide.contains("NEVER"),
        "memory guide must explicitly forbid freeing across allocators: {guide}"
    );
}

#[test]
fn summary_md_all_links_resolve() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let docs_src = workspace_root.join("docs/src");
    let summary = std::fs::read_to_string(docs_src.join("SUMMARY.md"))
        .expect("docs/src/SUMMARY.md must exist");

    let mut missing = Vec::new();
    for line in summary.lines() {
        // Extract path from markdown links like [Title](path.md)
        if let Some(start) = line.find("](") {
            let rest = &line[start + 2..];
            if let Some(end) = rest.find(')') {
                let target = &rest[..end];
                if target.is_empty() || !target.ends_with(".md") {
                    continue;
                }
                let full = docs_src.join(target);
                if !full.exists() {
                    missing.push(target.to_string());
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "SUMMARY.md references missing files: {missing:?}"
    );
}

const README_QUICKSTART_YAML: &str = r#"
version: "0.1.0"
modules:
  - name: contacts
    structs:
      - name: Contact
        fields:
          - name: id
            type: i64
          - name: name
            type: string
          - name: email
            type: "string?"
    functions:
      - name: create_contact
        params:
          - name: name
            type: string
          - name: email
            type: "string?"
        return: Contact
      - name: list_contacts
        params: []
        return: "[Contact]"
"#;

#[test]
fn readme_quickstart_yaml_parses_and_validates() {
    let mut api = weaveffi_ir::parse::parse_api_str(README_QUICKSTART_YAML, "yaml")
        .expect("README quickstart YAML should parse");
    weaveffi_core::validate::validate_api(&mut api)
        .expect("README quickstart YAML should validate");

    assert_eq!(api.modules.len(), 1);
    let m = &api.modules[0];
    assert_eq!(m.name, "contacts");
    assert_eq!(m.structs.len(), 1);
    assert_eq!(m.structs[0].name, "Contact");
    assert_eq!(m.structs[0].fields.len(), 3);
    assert_eq!(m.functions.len(), 2);
    assert_eq!(m.functions[0].name, "create_contact");
    assert_eq!(m.functions[1].name, "list_contacts");
}

#[test]
fn readme_quickstart_generates_c_header() {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();
    let yml_path = out_path.join("contacts.yml");
    std::fs::write(&yml_path, README_QUICKSTART_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.join("generated").to_str().unwrap(),
        ])
        .assert()
        .success();

    let header = std::fs::read_to_string(out_path.join("generated/c/weaveffi.h")).unwrap();

    assert!(
        header.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
        "opaque struct typedef missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_Contact_create("),
        "Contact_create missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_Contact_destroy("),
        "Contact_destroy missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_Contact_get_name("),
        "Contact_get_name missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_create_contact("),
        "create_contact missing: {header}"
    );
    assert!(
        header.contains("weaveffi_contacts_list_contacts("),
        "list_contacts missing: {header}"
    );
}

const GETTING_STARTED_YAML: &str = r#"
version: "0.1.0"
modules:
  - name: math
    structs:
      - name: Point
        fields:
          - { name: x, type: f64 }
          - { name: y, type: f64 }
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
"#;

#[test]
fn getting_started_yaml_parses_and_validates() {
    let mut api = weaveffi_ir::parse::parse_api_str(GETTING_STARTED_YAML, "yaml")
        .expect("getting-started YAML should parse");
    weaveffi_core::validate::validate_api(&mut api).expect("getting-started YAML should validate");

    assert_eq!(api.modules.len(), 1);
    assert_eq!(api.modules[0].name, "math");
    assert_eq!(api.modules[0].structs.len(), 1);
    assert_eq!(api.modules[0].structs[0].name, "Point");
    assert_eq!(api.modules[0].structs[0].fields.len(), 2);
    assert_eq!(api.modules[0].functions.len(), 1);
    assert_eq!(api.modules[0].functions[0].name, "add");
}

#[test]
fn getting_started_yaml_generates_all_targets() {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    let yml_path = out_path.join("weaveffi.yml");
    std::fs::write(&yml_path, GETTING_STARTED_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.join("generated").to_str().unwrap(),
            "--scaffold",
        ])
        .assert()
        .success();

    let gen = out_path.join("generated");
    assert!(gen.join("c/weaveffi.h").exists(), "missing c/weaveffi.h");
    assert!(
        gen.join("swift/Package.swift").exists(),
        "missing swift/Package.swift"
    );
    assert!(
        gen.join("node/types.d.ts").exists(),
        "missing node/types.d.ts"
    );
    assert!(gen.join("scaffold.rs").exists(), "missing scaffold.rs");

    let header = std::fs::read_to_string(gen.join("c/weaveffi.h")).unwrap();
    assert!(
        header.contains("weaveffi_math_add"),
        "C header should contain weaveffi_math_add"
    );
    assert!(
        header.contains("weaveffi_math_Point"),
        "C header should contain weaveffi_math_Point"
    );

    let dts = std::fs::read_to_string(gen.join("node/types.d.ts")).unwrap();
    assert!(
        dts.contains("export declare class Point"),
        "types.d.ts should contain Point class wrapper"
    );
    assert!(
        dts.contains("function math_add"),
        "types.d.ts should contain math_add function"
    );

    let scaffold = std::fs::read_to_string(gen.join("scaffold.rs")).unwrap();
    assert!(
        scaffold.contains("weaveffi_math_add"),
        "scaffold should contain weaveffi_math_add"
    );
    assert!(
        scaffold.contains("weaveffi_math_Point"),
        "scaffold should contain weaveffi_math_Point"
    );
}

#[test]
fn getting_started_yaml_validates_via_cli() {
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = out_dir.path().join("weaveffi.yml");
    std::fs::write(&yml_path, GETTING_STARTED_YAML).unwrap();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["validate", yml_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("Validation passed"));
}
