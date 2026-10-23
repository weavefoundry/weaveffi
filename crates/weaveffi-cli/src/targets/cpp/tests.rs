//! Target-specific unit tests: file naming and configuration, C++ keyword
//! and wrapper-member escaping, local names that dodge parameters, and doc
//! and deprecation text in C++ spelling. The `kitchen_sink` snapshot pins the
//! rest of the generated surface.

use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::codegen::test_model;
use crate::targets::cpp::{render_cpp_header, CppConfig, CppGenerator};
use crate::targets::Target;

fn header(model: &Model) -> String {
    render_cpp_header(model, "kv", "kv.h", "kv.hpp")
}

#[track_caller]
fn assert_contains(header: &str, needle: &str) {
    assert!(
        header.contains(needle),
        "expected the header to contain:\n{needle}\n---\n{header}"
    );
}

#[test]
fn identity_names_every_file_and_config_overrides_win() {
    let api = parse_api_str(
        r#"
version: "0.12.0"
modules:
  - name: m
    functions:
      - { name: ping, params: [], return: string }
"#,
        "yaml",
    )
    .expect("valid YAML");
    let model = validate(&api, &Identity::named("kv-store"), None).expect("valid API");
    let files = CppGenerator::from(CppConfig::default()).render(&model);
    let paths: Vec<String> = files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        paths,
        ["kv_store.h", "kv_store.hpp", "CMakeLists.txt", "README.md"]
    );
    let hpp = &files[1].contents;
    assert_contains(hpp, "#include \"kv_store.h\"");
    assert_contains(hpp, "namespace kv_store {");
    assert_contains(hpp, "return \"kv_store: \" + why;");
    assert!(
        !hpp.contains("class BufferWriter"),
        "no value buffers, no codec runtime"
    );

    let config = CppConfig {
        name: Some("acme::kv".into()),
        header_name: Some("acme_kv.hpp".into()),
        standard: Some("20".into()),
    };
    let files = CppGenerator::from(config).render(&model);
    assert_eq!(files[1].path, "acme_kv.hpp");
    assert_contains(&files[1].contents, "namespace acme::kv {");
    assert_contains(&files[1].contents, "#include \"kv_store.h\"");
    assert_contains(&files[2].contents, "cxx_std_20");
}

#[test]
fn keywords_and_wrapper_members_are_escaped() {
    let model = test_model(
        r#"
version: "0.12.0"
modules:
  - name: class
    structs:
      - name: Slot
        fields: [{ name: default, type: i32 }, { name: char8_t, type: "i32?" }]
    interfaces:
      - name: Box
        methods:
          - { name: handle, params: [], return: i32 }
          - { name: traits, params: [], return: i32 }
          - { name: delete, params: [{ name: register, type: i32 }], return: bool }
    functions:
      - { name: new, params: [{ name: namespace, type: string }], return: Slot }
"#,
    );
    let h = header(&model);
    assert_contains(&h, "namespace class_ {");
    assert_contains(&h, "int32_t default_{};");
    assert_contains(&h, "std::optional<int32_t> char8_t_{};");
    assert_contains(&h, "int32_t handle_() const;");
    assert_contains(&h, "int32_t traits_() const;");
    assert_contains(&h, "bool delete_(int32_t register_) const;");
    assert_contains(&h, "inline Slot new_(std::string_view namespace_) {");
}

#[test]
fn locals_dodge_parameter_names() {
    let model = test_model(
        r#"
version: "0.12.0"
modules:
  - name: m
    structs:
      - name: R
        fields: [{ name: x, type: i32 }]
    functions:
      - name: f
        params:
          - { name: err, type: i32 }
          - { name: result, type: R }
          - { name: result_buf, type: i32 }
        return: i64
"#,
    );
    let h = header(&model);
    assert_contains(&h, "const auto result_buf_ = detail::encode(result);");
    assert_contains(&h, "kv_error err_{};");
    assert_contains(
        &h,
        "auto result_ = kv_m_f(err, result_buf_.data(), result_buf_.size(), result_buf, &err_);",
    );
    assert_contains(&h, "detail::check<InternalError>(err_);");
}

#[test]
fn doc_identifiers_and_deprecations_use_cpp_spellings() {
    let model = test_model(
        r#"
version: "0.12.0"
modules:
  - name: m
    errors:
      - name: StoreErrors
        codes: [{ name: NOT_FOUND, code: 1, message: missing }]
    functions:
      - name: legacy
        doc: "Fails with `StoreErrors` (`NOT_FOUND`) unlike `newOne`."
        params: []
        return: i32
        throws: StoreErrors
        deprecated: "Use `newOne` instead"
      - { name: newOne, params: [], return: i32 }
"#,
    );
    let h = header(&model);
    assert_contains(
        &h,
        "Fails with `StoreError` (`NotFoundError`) unlike `new_one`.",
    );
    assert_contains(&h, "[[deprecated(\"Use `new_one` instead\")]]");
    assert_contains(&h, "inline int32_t new_one() {");
}

#[test]
fn error_fields_never_hide_the_exception_members() {
    let model = test_model(
        r#"
version: "0.12.0"
modules:
  - name: m
    errors:
      - name: Oops
        codes:
          - name: Failed
            code: 1
            message: failed
            fields:
              - { name: message, type: string }
              - { name: code, type: i32 }
              - { name: what, type: string }
    functions:
      - { name: f, params: [], return: i32, throws: Oops }
"#,
    );
    let h = header(&model);
    assert_contains(&h, "std::string message;");
    assert_contains(&h, "int32_t code_;");
    assert_contains(&h, "std::string what_;");
    assert_contains(
        &h,
        "FailedError(const std::string& message_, std::string message, int32_t code_, std::string what_) \
         : OopsError(1, message_), message(std::move(message)), code_(code_), what_(std::move(what_)) {}",
    );
    assert_contains(
        &h,
        "report_with_fields(out_err, e, e.message, e.code_, e.what_);",
    );
}

#[test]
fn only_value_initializable_members_get_default_initializers() {
    let model = test_model(
        r#"
version: "0.12.0"
modules:
  - name: m
    interfaces:
      - name: Token
        methods: [{ name: id, params: [], return: i64 }]
    structs:
      - name: Holder
        fields: [{ name: token, type: Token }, { name: spare, type: "Token?" }]
      - name: Outer
        fields: [{ name: holder, type: Holder }, { name: many, type: "[Holder]" }, { name: n, type: u8 }]
    functions:
      - { name: f, params: [{ name: o, type: Outer }], return: i32 }
"#,
    );
    let h = header(&model);
    assert_contains(&h, "    Token token;\n");
    assert_contains(&h, "    std::optional<Token> spare{};\n");
    assert_contains(&h, "    Holder holder;\n");
    assert_contains(&h, "    std::vector<Holder> many{};\n");
    assert_contains(&h, "    uint8_t n{};\n");
}
