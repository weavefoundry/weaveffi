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

fn read_doc(relative_path: &str) -> String {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::fs::read_to_string(workspace_root.join(relative_path))
        .unwrap_or_else(|e| panic!("{relative_path} must exist: {e}"))
}

/// Extract the first fenced ```rust``` code block that appears after the
/// given markdown heading (exact string match, including the `##` prefix).
fn extract_rust_block_after_heading(md: &str, heading: &str) -> String {
    let after = md
        .split_once(heading)
        .unwrap_or_else(|| panic!("heading not found: {heading}"))
        .1;
    let start = after
        .find("```rust")
        .unwrap_or_else(|| panic!("no ```rust``` block after heading {heading}"));
    let body = &after[start + "```rust".len()..];
    let nl = body.find('\n').expect("unterminated code fence");
    let body = &body[nl + 1..];
    let end = body.find("```").expect("unterminated code fence");
    body[..end].to_string()
}

#[test]
fn extract_guide_documents_all_attributes() {
    let guide = read_doc("docs/src/guides/extract.md");
    let required = [
        "#[weaveffi_export]",
        "#[weaveffi_export(async)]",
        "#[weaveffi_export(cancellable)]",
        "since = \"",
        "#[weaveffi_struct]",
        "#[weaveffi_struct(builder)]",
        "#[weaveffi_enum]",
        "#[weaveffi_callback]",
        "#[weaveffi_callback = \"",
        "#[weaveffi_listener(event = \"",
        "#[weaveffi_typed_handle = \"",
        "#[weaveffi_default = \"",
        "#[deprecated",
    ];
    for attr in required {
        assert!(
            guide.contains(attr),
            "extract guide must document {attr}: {guide}"
        );
    }
}

#[test]
fn extract_guide_documents_required_limitations() {
    let guide = read_doc("docs/src/guides/extract.md");
    assert!(
        guide.contains("## Limitations"),
        "extract guide must have a Limitations section: {guide}"
    );
    let required = [
        "Generic functions",
        "Trait implementations",
        "Lifetime parameters",
        "&str",
        "&[u8]",
    ];
    for item in required {
        assert!(
            guide.contains(item),
            "extract guide Limitations must mention {item}: {guide}"
        );
    }
}

#[test]
fn extract_guide_contacts_example_matches_sample_yml() {
    let guide = read_doc("docs/src/guides/extract.md");
    let rust = extract_rust_block_after_heading(&guide, "## Complete example");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_path = dir.path().join("lib.rs");
    std::fs::write(&src_path, &rust).unwrap();

    let output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args(["extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run extract");
    assert!(
        output.status.success(),
        "extract failed on guide example: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let extracted: weaveffi_ir::ir::Api =
        serde_yaml::from_slice(&output.stdout).expect("extract output should parse as Api");

    let sample_yml = read_doc("samples/contacts/contacts.yml");
    let sample: weaveffi_ir::ir::Api =
        serde_yaml::from_str(&sample_yml).expect("contacts.yml should parse as Api");

    assert_eq!(
        extracted.modules, sample.modules,
        "guide example must extract to the same modules as samples/contacts/contacts.yml"
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

/// Every sample README must explain the same four things: the features
/// demonstrated, the IDL highlights, the generate command, and what to
/// look for in the generated output. These tests guard that contract so a
/// future refactor does not quietly drop a section.
fn assert_sample_readme_sections(sample: &str) {
    let readme = read_doc(&format!("samples/{sample}/README.md"));
    let required = [
        "## What this sample demonstrates",
        "## IDL highlights",
        "## Generate",
        "## What to look for in the generated output",
    ];
    for heading in required {
        assert!(
            readme.contains(heading),
            "samples/{sample}/README.md must contain `{heading}`: {readme}"
        );
    }
}

#[test]
fn contacts_readme_has_required_sections() {
    assert_sample_readme_sections("contacts");
    let readme = read_doc("samples/contacts/README.md");
    assert!(
        readme.contains("contacts.yml"),
        "contacts README must reference its IDL file: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml"),
        "contacts README must include the generate command: {readme}"
    );
}

#[test]
fn inventory_readme_has_required_sections() {
    assert_sample_readme_sections("inventory");
    let readme = read_doc("samples/inventory/README.md");
    assert!(
        readme.contains("inventory.yml"),
        "inventory README must reference its IDL file: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/inventory/inventory.yml"),
        "inventory README must include the generate command: {readme}"
    );
    for module in ["products", "orders"] {
        assert!(
            readme.contains(module),
            "inventory README must mention the `{module}` module: {readme}"
        );
    }
}

#[test]
fn async_demo_readme_has_required_sections() {
    assert_sample_readme_sections("async-demo");
    let readme = read_doc("samples/async-demo/README.md");
    assert!(
        readme.contains("async_demo.yml"),
        "async-demo README must reference its IDL file: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/async-demo/async_demo.yml"),
        "async-demo README must include the generate command: {readme}"
    );
    assert!(
        readme.contains("async: true"),
        "async-demo README must call out the async IDL flag: {readme}"
    );
    assert!(
        readme.contains("_async"),
        "async-demo README must document the _async C ABI suffix: {readme}"
    );
}

#[test]
fn events_readme_has_required_sections() {
    assert_sample_readme_sections("events");
    let readme = read_doc("samples/events/README.md");
    assert!(
        readme.contains("events.yml"),
        "events README must reference its IDL file: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/events/events.yml"),
        "events README must include the generate command: {readme}"
    );
    for token in ["callback", "listener", "iter"] {
        assert!(
            readme.contains(token),
            "events README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn node_addon_readme_has_required_sections() {
    assert_sample_readme_sections("node-addon");
    let readme = read_doc("samples/node-addon/README.md");
    assert!(
        readme.contains("calculator.yml"),
        "node-addon README must reference the calculator IDL it consumes: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml"),
        "node-addon README must include the generate command it depends on: {readme}"
    );
    for token in ["napi", "libloading", "WEAVEFFI_LIB"] {
        assert!(
            readme.contains(token),
            "node-addon README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn wasm_browser_example_files_exist() {
    for file in [
        "examples/wasm/browser/index.html",
        "examples/wasm/browser/app.js",
        "examples/wasm/browser/build.sh",
        "examples/wasm/browser/serve.sh",
        "examples/wasm/browser/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "WASM browser example is missing {file}"
        );
    }
}

#[test]
fn wasm_browser_app_uses_worker_generated_loader_and_calculator() {
    let app = read_doc("examples/wasm/browser/app.js");
    for token in [
        "new Worker(new URL(\"./app.js?worker\", import.meta.url), { type: \"module\" })",
        "import(\"../../generated/wasm/weaveffi_wasm.js\")",
        "loadWeaveffiWasm(wasmUrl)",
        "api.calculator.add(args.a, args.b)",
        "api.calculator.echo(args.s)",
        "api.calculator.div(1, 0)",
        "ok: false,",
    ] {
        assert!(app.contains(token), "app.js must contain `{token}`: {app}");
    }
}

#[test]
fn wasm_browser_index_exposes_calculator_ui() {
    let html = read_doc("examples/wasm/browser/index.html");
    for token in [
        "weaveffi_wasm.js",
        "id=\"add-button\"",
        "id=\"echo-button\"",
        "id=\"error-button\"",
        "type=\"module\" src=\"./app.js\"",
    ] {
        assert!(
            html.contains(token),
            "index.html must contain `{token}`: {html}"
        );
    }
}

#[test]
fn wasm_browser_scripts_and_readme_document_build_and_serve() {
    let build = read_doc("examples/wasm/browser/build.sh");
    let build_pos = build
        .find("cargo build -p calculator --target wasm32-unknown-unknown --release")
        .unwrap_or_else(|| panic!("build.sh must compile the calculator cdylib: {build}"));
    let generate_pos = build
        .find("cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml")
        .unwrap_or_else(|| panic!("build.sh must generate the WASM wrapper: {build}"));
    assert!(
        build_pos < generate_pos,
        "build.sh must compile the calculator cdylib before generating bindings: {build}"
    );

    let serve = read_doc("examples/wasm/browser/serve.sh");
    assert!(
        serve.contains("python3 -m http.server 8080"),
        "serve.sh must wrap python's static server: {serve}"
    );

    let readme = read_doc("examples/wasm/browser/README.md");
    for token in [
        "./examples/wasm/browser/build.sh",
        "wasm32-unknown-unknown",
        "examples/generated/wasm/",
        "python3 -m http.server 8080",
        "http://localhost:8080/examples/wasm/browser/",
    ] {
        assert!(
            readme.contains(token),
            "README.md must contain `{token}`: {readme}"
        );
    }
}

#[test]
fn cpp_calculator_example_files_exist() {
    for file in [
        "examples/cpp/calculator/CMakeLists.txt",
        "examples/cpp/calculator/main.cpp",
        "examples/cpp/calculator/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "C++ calculator example is missing {file}"
        );
    }
}

#[test]
fn cpp_calculator_main_cpp_exercises_generated_wrappers() {
    let main = read_doc("examples/cpp/calculator/main.cpp");
    assert!(
        main.contains("#include \"weaveffi.hpp\""),
        "main.cpp must include the generated header: {main}"
    );
    for call in [
        "weaveffi::calculator_add",
        "weaveffi::calculator_mul",
        "weaveffi::calculator_div",
        "weaveffi::calculator_echo",
        "weaveffi::WeaveFFIError",
    ] {
        assert!(main.contains(call), "main.cpp must call `{call}`: {main}");
    }
}

#[test]
fn cpp_calculator_cmake_links_calculator_cdylib() {
    let cmake = read_doc("examples/cpp/calculator/CMakeLists.txt");
    assert!(
        cmake.contains("CALCULATOR_LIB_DIR"),
        "CMakeLists.txt must expose a CALCULATOR_LIB_DIR override: {cmake}"
    );
    for token in [
        "libcalculator.dylib",
        "libcalculator.so",
        "calculator.dll",
        "add_subdirectory(../../../generated/cpp",
        "add_executable(calculator main.cpp)",
    ] {
        assert!(
            cmake.contains(token),
            "CMakeLists.txt must contain `{token}`: {cmake}"
        );
    }
}

#[test]
fn cpp_calculator_readme_documents_build_and_run() {
    let readme = read_doc("examples/cpp/calculator/README.md");
    assert!(
        readme.contains("cargo build -p calculator"),
        "README must instruct building the calculator cdylib: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml"),
        "README must include the calculator generate command: {readme}"
    );
    for token in [
        "cmake -S . -B build",
        "cmake --build build",
        "./build/calculator",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn dart_contacts_example_files_exist() {
    for file in [
        "examples/dart/contacts/bin/main.dart",
        "examples/dart/contacts/pubspec.yaml",
        "examples/dart/contacts/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "Dart contacts example is missing {file}"
        );
    }
}

#[test]
fn dart_contacts_main_exercises_generated_bindings() {
    let main = read_doc("examples/dart/contacts/bin/main.dart");
    assert!(
        main.contains("import 'package:weaveffi/weaveffi.dart';"),
        "main.dart must import the generated package: {main}"
    );
    for call in [
        "createContact(",
        "countContacts()",
        "listContacts()",
        "getContact(",
        "deleteContact(",
        "ContactType.personal",
        "ContactType.work",
    ] {
        assert!(main.contains(call), "main.dart must call `{call}`: {main}");
    }
    // The example must show Dart's equivalent of RAII: explicit `dispose()`
    // on every `Contact` obtained from the bindings, guarded by `finally` so
    // native handles still release on exception paths.
    assert!(
        main.contains(".dispose()"),
        "main.dart must demonstrate Contact.dispose(): {main}"
    );
    assert!(
        main.contains("finally"),
        "main.dart must dispose handles in a finally block: {main}"
    );
}

#[test]
fn dart_contacts_pubspec_is_pure_dart_and_depends_on_generated() {
    let pubspec = read_doc("examples/dart/contacts/pubspec.yaml");
    assert!(
        pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
        "pubspec must pin a modern pure-Dart SDK range: {pubspec}"
    );
    assert!(
        !pubspec.contains("flutter:"),
        "pubspec must not require Flutter so `dart pub get` works: {pubspec}"
    );
    assert!(
        pubspec.contains("path: ../../../generated/dart"),
        "pubspec must depend on the generated dart package via path: {pubspec}"
    );
}

#[test]
fn dart_contacts_readme_documents_build_and_run() {
    let readme = read_doc("examples/dart/contacts/README.md");
    assert!(
        readme.contains("cargo build -p contacts"),
        "README must instruct building the contacts cdylib: {readme}"
    );
    assert!(
        readme.contains("cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml"),
        "README must include the contacts generate command: {readme}"
    );
    for token in [
        "dart pub get",
        "dart run bin/main.dart",
        "DYLD_LIBRARY_PATH=../../../target/debug",
        "LD_LIBRARY_PATH=../../../target/debug",
        "libweaveffi.dylib",
        "libweaveffi.so",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn go_contacts_example_files_exist() {
    for file in [
        "examples/go/contacts/main.go",
        "examples/go/contacts/go.mod",
        "examples/go/contacts/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "Go contacts example is missing {file}"
        );
    }
}

#[test]
fn go_contacts_main_exercises_generated_bindings() {
    let main = read_doc("examples/go/contacts/main.go");
    assert!(
        main.contains("weaveffi \"github.com/example/weaveffi\""),
        "main.go must import the generated Go module: {main}"
    );
    for call in [
        "weaveffi.ContactsCreateContact(",
        "weaveffi.ContactsCountContacts()",
        "weaveffi.ContactsListContacts()",
        "weaveffi.ContactsGetContact(",
        "weaveffi.ContactsDeleteContact(",
        "weaveffi.ContactTypePersonal",
        "weaveffi.ContactTypeWork",
    ] {
        assert!(main.contains(call), "main.go must call `{call}`: {main}");
    }
    for close_call in ["contact.Close()", "fetched.Close()"] {
        assert!(
            main.contains(close_call),
            "main.go must explicitly close generated structs with `{close_call}`: {main}"
        );
    }
}

#[test]
fn go_contacts_go_mod_replaces_generated_module() {
    let go_mod = read_doc("examples/go/contacts/go.mod");
    assert!(
        go_mod.contains("module github.com/example/weaveffi-go-contacts"),
        "go.mod must declare the consumer example module: {go_mod}"
    );
    assert!(
        go_mod.contains("require github.com/example/weaveffi v0.0.0"),
        "go.mod must require the generated module path: {go_mod}"
    );
    assert!(
        go_mod.contains("replace github.com/example/weaveffi => ../../generated/go"),
        "go.mod must replace the generated module with ../../generated/go: {go_mod}"
    );
}

#[test]
fn go_contacts_readme_documents_cgo_build_and_run() {
    let readme = read_doc("examples/go/contacts/README.md");
    assert!(
        readme.contains("cargo build -p contacts"),
        "README must instruct building the contacts cdylib: {readme}"
    );
    for token in [
        "cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o examples/generated --target c",
        "cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o examples/generated --target go",
        "replace github.com/example/weaveffi => ../../generated/go",
        "CGO_CFLAGS=\"-I$ROOT/examples/generated/c\"",
        "CGO_LDFLAGS=\"-L$ROOT/target/debug -lweaveffi\"",
        "go run .",
        "libweaveffi.dylib",
        "libweaveffi.so",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn go_sqlite_contacts_example_files_exist() {
    for file in [
        "examples/go/sqlite-contacts/main.go",
        "examples/go/sqlite-contacts/go.mod",
        "examples/go/sqlite-contacts/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "Go SQLite contacts example is missing {file}"
        );
    }
}

#[test]
fn go_sqlite_contacts_main_exercises_async_and_iterator_channels() {
    let main = read_doc("examples/go/sqlite-contacts/main.go");
    assert!(
        main.contains("weaveffi \"github.com/example/weaveffi\""),
        "main.go must import the generated Go module: {main}"
    );
    assert!(
        main.contains("context.WithTimeout("),
        "main.go must pass context to cancellable async create_contact: {main}"
    );
    for call in [
        "<-weaveffi.ContactsCreateContact(",
        "<-weaveffi.ContactsUpdateContact(",
        "<-weaveffi.ContactsFindContact(",
        "<-weaveffi.ContactsCountContacts(",
        "<-weaveffi.ContactsDeleteContact(",
        "weaveffi.ContactsListContacts(nil)",
        "list_contacts := func() <-chan *weaveffi.Contact",
        "weaveffi.StatusActive",
    ] {
        assert!(main.contains(call), "main.go must call `{call}`: {main}");
    }
    assert!(
        main.contains("for contact := range list_contacts() {"),
        "main.go must consume the iterator channel with range over list_contacts(): {main}"
    );
    assert!(
        main.contains("contact.Close()"),
        "main.go must close contacts yielded by the iterator channel: {main}"
    );
}

#[test]
fn go_sqlite_contacts_go_mod_replaces_generated_module() {
    let go_mod = read_doc("examples/go/sqlite-contacts/go.mod");
    assert!(
        go_mod.contains("module github.com/example/weaveffi-go-sqlite-contacts"),
        "go.mod must declare the consumer example module: {go_mod}"
    );
    assert!(
        go_mod.contains("require github.com/example/weaveffi v0.0.0"),
        "go.mod must require the generated module path: {go_mod}"
    );
    assert!(
        go_mod.contains("replace github.com/example/weaveffi => ../../generated/go"),
        "go.mod must replace the generated module with ../../generated/go: {go_mod}"
    );
}

#[test]
fn go_sqlite_contacts_readme_documents_cgo_async_and_iterator_run() {
    let readme = read_doc("examples/go/sqlite-contacts/README.md");
    assert!(
        readme.contains("cargo build -p sqlite-contacts"),
        "README must instruct building the sqlite-contacts cdylib: {readme}"
    );
    for token in [
        "cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o examples/generated --target c",
        "cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o examples/generated --target go",
        "replace github.com/example/weaveffi => ../../generated/go",
        "CGO_CFLAGS=\"-I$ROOT/examples/generated/c\"",
        "CGO_LDFLAGS=\"-L$ROOT/target/debug -lweaveffi\"",
        "<-weaveffi.ContactsCreateContact(ctx, ...)",
        "for contact := range list_contacts() { ... }",
        "go run .",
        "libweaveffi.dylib",
        "libweaveffi.so",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn dart_flutter_contacts_example_files_exist() {
    for file in [
        "examples/dart/flutter-contacts/lib/main.dart",
        "examples/dart/flutter-contacts/test/widget_test.dart",
        "examples/dart/flutter-contacts/pubspec.yaml",
        "examples/dart/flutter-contacts/analysis_options.yaml",
        "examples/dart/flutter-contacts/tool/flutter_ci.sh",
        "examples/dart/flutter-contacts/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "Dart Flutter contacts example is missing {file}"
        );
    }
}

#[test]
fn dart_flutter_contacts_app_uses_generated_bindings() {
    let main = read_doc("examples/dart/flutter-contacts/lib/main.dart");
    assert!(
        main.contains("import 'package:weaveffi/weaveffi.dart' as weaveffi;"),
        "Flutter main.dart must import the generated package: {main}"
    );
    for token in [
        "MaterialApp(",
        "ListView.separated(",
        "weaveffi.countContacts()",
        "weaveffi.createContact(",
        "weaveffi.listContacts()",
        "weaveffi.ContactType.personal",
        "contact.dispose()",
        "finally",
    ] {
        assert!(
            main.contains(token),
            "Flutter main.dart must contain `{token}`: {main}"
        );
    }
}

#[test]
fn dart_flutter_contacts_pubspec_is_flutter_app() {
    let pubspec = read_doc("examples/dart/flutter-contacts/pubspec.yaml");
    assert!(
        pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
        "pubspec must pin a modern Dart SDK range: {pubspec}"
    );
    assert!(
        pubspec.contains("flutter:\n    sdk: flutter"),
        "pubspec must declare a Flutter SDK dependency: {pubspec}"
    );
    assert!(
        pubspec.contains("path: ../../../generated/dart"),
        "pubspec must depend on the generated dart package via path: {pubspec}"
    );
    assert!(
        pubspec.contains("flutter_test:\n    sdk: flutter"),
        "pubspec must include flutter_test for widget tests: {pubspec}"
    );
}

#[test]
fn dart_flutter_contacts_widget_test_renders_contact_rows() {
    let test = read_doc("examples/dart/flutter-contacts/test/widget_test.dart");
    for token in [
        "testWidgets(",
        "ContactsApp(",
        "ContactRow(",
        "find.text('Alice Smith')",
        "find.text('No email')",
    ] {
        assert!(
            test.contains(token),
            "widget_test.dart must contain `{token}`: {test}"
        );
    }
}

#[test]
fn dart_flutter_contacts_ci_script_is_optional() {
    let script = read_doc("examples/dart/flutter-contacts/tool/flutter_ci.sh");
    for token in [
        "command -v flutter",
        "skipping optional Flutter contacts example",
        "exit 0",
        "cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml",
        "flutter analyze",
        "flutter test",
        "flutter build bundle",
    ] {
        assert!(
            script.contains(token),
            "flutter_ci.sh must contain `{token}`: {script}"
        );
    }
}

#[test]
fn dart_flutter_contacts_readme_documents_optional_build() {
    let readme = read_doc("examples/dart/flutter-contacts/README.md");
    for token in [
        "cargo build -p contacts",
        "cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml",
        "flutter pub get",
        "flutter run",
        "DYLD_LIBRARY_PATH=../../../target/debug",
        "LD_LIBRARY_PATH=../../../target/debug",
        "Optional CI",
        "tool/flutter_ci.sh",
        "Flutter SDK is not",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}

#[test]
fn dart_sqlite_contacts_example_files_exist() {
    for file in [
        "examples/dart/sqlite-contacts/bin/main.dart",
        "examples/dart/sqlite-contacts/pubspec.yaml",
        "examples/dart/sqlite-contacts/README.md",
    ] {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            workspace_root.join(file).exists(),
            "Dart SQLite contacts example is missing {file}"
        );
    }
}

#[test]
fn dart_sqlite_contacts_main_exercises_async_generated_bindings() {
    let main = read_doc("examples/dart/sqlite-contacts/bin/main.dart");
    assert!(
        main.contains("import 'package:weaveffi/weaveffi.dart';"),
        "main.dart must import the generated package: {main}"
    );
    assert!(
        main.contains("Future<void> main() async"),
        "main.dart must expose an async entry point: {main}"
    );
    for call in [
        "await createContact(",
        "await findContact(",
        "await updateContact(",
        "await countContacts(",
        "await deleteContact(",
        "Status.active",
    ] {
        assert!(main.contains(call), "main.dart must call `{call}`: {main}");
    }
    assert!(
        main.contains(".dispose()"),
        "main.dart must dispose Contact handles: {main}"
    );
    assert!(
        main.contains("finally"),
        "main.dart must release handles in a finally block: {main}"
    );
}

#[test]
fn dart_sqlite_contacts_pubspec_is_pure_dart_and_depends_on_generated() {
    let pubspec = read_doc("examples/dart/sqlite-contacts/pubspec.yaml");
    assert!(
        pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
        "pubspec must pin a modern pure-Dart SDK range: {pubspec}"
    );
    assert!(
        !pubspec.contains("flutter:"),
        "pubspec must not require Flutter so `dart pub get` works: {pubspec}"
    );
    assert!(
        pubspec.contains("path: ../../../generated/dart"),
        "pubspec must depend on the generated dart package via path: {pubspec}"
    );
}

#[test]
fn dart_sqlite_contacts_readme_documents_build_generate_and_run() {
    let readme = read_doc("examples/dart/sqlite-contacts/README.md");
    assert!(
        readme.contains("cargo build -p sqlite-contacts"),
        "README must instruct building the sqlite-contacts cdylib: {readme}"
    );
    assert!(
        readme.contains(
            "cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml"
        ),
        "README must include the sqlite-contacts generate command: {readme}"
    );
    for token in [
        "dart pub get",
        "dart run bin/main.dart",
        "DYLD_LIBRARY_PATH=../../../target/debug",
        "LD_LIBRARY_PATH=../../../target/debug",
        "libweaveffi.dylib",
        "libweaveffi.so",
        "await createContact",
    ] {
        assert!(
            readme.contains(token),
            "README must mention `{token}`: {readme}"
        );
    }
}
