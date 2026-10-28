//! Target-specific unit tests: file naming and configuration, C++ keyword
//! and wrapper-member escaping, local names that dodge parameters, how each
//! value crosses, the contract rows, and doc and deprecation text in C++
//! spelling. The `kitchen_sink` snapshot pins the rest of the generated
//! surface.

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
version: "0.13.0"
functions:
  - { name: ping, return: string }
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
    assert_contains(hpp, "inline std::string ping() {");
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
version: "0.13.0"
records:
  - name: Slot
    fields: [{ name: default, type: i32 }, { name: char8_t, type: "i32?" }]
interfaces:
  - name: Box
    methods:
      - { name: handle, return: i32 }
      - { name: traits, return: i32 }
      - { name: delete, params: [{ name: register, type: i32 }], return: bool }
callbacks:
  - name: Codec
    methods:
      - { name: encode, params: [{ name: decode, type: Slot }, { name: std, type: string }], return: Slot }
functions:
  - { name: new, params: [{ name: namespace, type: string }], return: Slot }
  - { name: detail, params: [{ name: adopt, type: Box }, { name: codec, type: Codec }], return: i32 }
"#,
    );
    let h = header(&model);
    // Names that would hide what the generated bodies call gain a `_`.
    assert_contains(
        &h,
        "inline int32_t detail_(const Box& adopt_, std::shared_ptr<Codec> codec) {",
    );
    assert_contains(
        &h,
        "static kv_run encode_(void* ctx, kv_run decode_, kv_run std_, kv_error* out_err) {",
    );
    assert_contains(&h, "auto decode_arg = decode<Slot>(decode_);");
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
version: "0.13.0"
records:
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
        "auto result_ = kv_f(err, result_buf_.view(), result_buf, &err_);",
    );
    assert_contains(&h, "detail::check<InternalError>(err_);");
}

#[test]
fn every_value_crosses_as_one_c_value() {
    let model = test_model(
        r#"
version: "0.13.0"
enums:
  - name: Mode
    variants: [{ name: Fast }, { name: Slow }]
interfaces:
  - name: Store
    methods: [{ name: id, return: i64 }]
callbacks:
  - name: Sink
    methods:
      - { name: put, params: [{ name: key, type: string }, { name: n, type: "u32?" }], return: "[f64]" }
      - { name: pick, params: [{ name: store, type: "Store?" }], return: "Mode?" }
functions:
  - name: mix
    params:
      - { name: s, type: string }
      - { name: b, type: bytes }
      - { name: xs, type: "[i32]" }
      - { name: o, type: "i64?" }
      - { name: m, type: "Mode?" }
      - { name: st, type: "Store?" }
      - { name: sink, type: "Sink?" }
    return: "[u16]"
  - { name: opt, return: "bool?" }
  - { name: stream, return: "iter<string?>" }
  - { name: later, return: "f32?", async: true, cancellable: true }
"#,
    );
    let h = header(&model);
    assert_contains(&h, "auto sink_ctx = detail::lend(std::move(sink));");
    assert_contains(
        &h,
        "auto result = kv_mix(detail::string_run(s), detail::bytes_run(b), detail::slice_run(xs), \
         detail::to_opt<kv_opt_i64>(o), detail::to_opt<kv_opt_i32>(m), detail::handle_of(st), \
         kv_Sink{sink_ctx.release(), sink_vtable}, &err);",
    );
    assert_contains(&h, "return detail::take_slice<uint16_t>(result);");
    assert_contains(&h, "return detail::from_opt<bool>(result);");
    // An iterator item is one C value; a buffered optional string is a run.
    assert_contains(&h, "kv_run out_item{};");
    assert_contains(
        &h,
        "if (kv_StreamIterator_next(static_cast<kv_StreamIterator*>(raw), &out_item, &next_err) == 0) {",
    );
    assert_contains(
        &h,
        "item.emplace(detail::take<std::optional<std::string>>(out_item));",
    );
    assert_contains(
        &h,
        "kv_later(cancel_token.handle(), [](void* context, kv_error* err, kv_opt_f32 result) {",
    );
    assert_contains(&h, "return detail::from_opt<float>(result); });");
    // Callback arguments are borrowed; returns are allocated or packed.
    assert_contains(
        &h,
        "static kv_run put(void* ctx, kv_run key, kv_opt_u32 n, kv_error* out_err) {",
    );
    assert_contains(
        &h,
        "return give_slice(implementation<Sink>(ctx).put(borrow_string(key), from_opt<uint32_t>(n)));",
    );
    assert_contains(
        &h,
        "std::optional<Store> store_arg = adopt_optional<Store>(store);",
    );
    assert_contains(
        &h,
        "return to_opt<kv_opt_i32>(implementation<Sink>(ctx).pick(std::move(store_arg)));",
    );
    assert_contains(&h, "sizeof(kv_Sink_vtable),");
}

#[test]
fn every_declaration_has_a_contract_row() {
    let model = test_model(
        r#"
version: "0.13.0"
errors:
  - name: Oops
    codes: [{ name: Broke, code: 1, message: broke }]
functions:
  - { name: get, params: [{ name: key, type: string }], return: i64, throws: Oops }
"#,
    );
    let h = header(&model);
    let rows = crate::codegen::contract::rows(&model);
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_contains(
            &h,
            &format!(
                "{{&{}, {}ull, \"{}\"}}, // {}",
                row.symbol,
                crate::codegen::contract::hex(row.hash),
                row.path,
                row.signature
            ),
        );
    }
    assert_contains(
        &h,
        "if (std::string why = detail::contract_mismatch(detail::contract_rows); !why.empty()) {",
    );
    assert_contains(&h, "if (abi != KV_ABI_VERSION) {");

    // C++ has no empty arrays: an API without declarations has no rows.
    let empty = validate(
        &weaveffi_model::ir::Api::new(),
        &Identity::named("kv"),
        None,
    )
    .expect("valid API");
    let h = header(&empty);
    assert!(!h.contains("contract_rows"), "{h}");
}

#[test]
fn doc_identifiers_and_deprecations_use_cpp_spellings() {
    let model = test_model(
        r#"
version: "0.13.0"
errors:
  - name: StoreErrors
    codes: [{ name: NOT_FOUND, code: 1, message: missing }]
functions:
  - name: legacy
    doc: "Fails with `StoreErrors` (`NOT_FOUND`) unlike `newOne`."
    return: i32
    throws: StoreErrors
    deprecated: "Use `newOne` instead"
  - { name: newOne, return: i32 }
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
version: "0.13.0"
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
  - { name: f, return: i32, throws: Oops }
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
version: "0.13.0"
interfaces:
  - name: Token
    methods: [{ name: id, return: i64 }]
records:
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
