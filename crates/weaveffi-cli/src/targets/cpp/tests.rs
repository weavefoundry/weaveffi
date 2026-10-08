//! Unit tests for the behavior the snapshots don't make obvious: the
//! contract table the library check embeds, the vtable layout, callback
//! returns and errors, optional callbacks, the trap policy, one codec per
//! composite type, and the ordering that keeps adopted objects from leaking.

use camino::Utf8Path;
use weaveffi_model::contract;
use weaveffi_model::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function,
    InterfaceDef, Module, Param, StructDef, StructField, TypeRef,
};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::ty::Prim;
use weaveffi_model::validate::validate;

use crate::backend::LanguageBackend;
use crate::targets::cpp::{render_cpp_header, CppConfig, CppGenerator};

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
    }
}

fn throwing(f: Function) -> Function {
    Function { throws: true, ..f }
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.into())
}

fn optional(ty: TypeRef) -> TypeRef {
    TypeRef::Optional(Box::new(ty))
}

fn list(ty: TypeRef) -> TypeRef {
    TypeRef::List(Box::new(ty))
}

fn string() -> TypeRef {
    TypeRef::Prim(Prim::String)
}

/// One module with an interface, a record holding objects, a C-style enum,
/// an error domain with a payload-carrying code, and a callback interface
/// whose methods take objects and return every family (one of them
/// `throws`), passed both required and optional.
fn fixture() -> Api {
    let store = InterfaceDef {
        name: "Store".into(),
        doc: None,
        deprecated: None,
        constructors: vec![func("new", vec![param("path", string())], None)],
        methods: vec![
            throwing(func("save", vec![param("contact", named("Contact"))], None)),
            func("count", vec![], Some(TypeRef::Prim(Prim::U32))),
        ],
        statics: vec![],
    };
    let listener = CallbackInterfaceDef {
        name: "Listener".into(),
        doc: None,
        deprecated: None,
        methods: vec![
            func(
                "on_message",
                vec![
                    param("text", string()),
                    param("contact", named("Contact")),
                    param("store", named("Store")),
                ],
                Some(TypeRef::Prim(Prim::Bool)),
            ),
            func(
                "on_reset",
                vec![param("alt", optional(named("Store")))],
                None,
            ),
            func(
                "pick",
                vec![param("color", named("Color"))],
                Some(named("Color")),
            ),
            throwing(func("label", vec![], Some(string()))),
            func("blob", vec![], Some(TypeRef::Prim(Prim::Bytes))),
            func("latest", vec![], Some(optional(named("Contact")))),
            func("home", vec![], Some(named("Store"))),
            func("spare", vec![], Some(optional(named("Store")))),
        ],
    };
    let variant = |name: &str, value: i32| EnumVariant {
        name: name.into(),
        value,
        doc: None,
        fields: vec![],
    };
    Api {
        version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
        modules: vec![Module {
            name: "kv".into(),
            doc: None,
            functions: vec![
                func("count", vec![], Some(TypeRef::Prim(Prim::U32))),
                throwing(func("fetch", vec![], Some(named("Contact")))),
                func(
                    "subscribe",
                    vec![param("listener", named("Listener"))],
                    None,
                ),
                func(
                    "maybe_subscribe",
                    vec![param("listener", optional(named("Listener")))],
                    None,
                ),
                func(
                    "first",
                    vec![param("stores", list(named("Store")))],
                    Some(named("Store")),
                ),
            ],
            interfaces: vec![store],
            callback_interfaces: vec![listener],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                deprecated: None,
                fields: vec![
                    field("name", string()),
                    field("store", named("Store")),
                    field("mirrors", list(named("Store"))),
                    field("primary", optional(named("Store"))),
                ],
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                deprecated: None,
                variants: vec![variant("Red", 0), variant("Blue", 1)],
            }],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "NOT_FOUND".into(),
                        code: 1,
                        message: "not found".into(),
                        doc: None,
                        fields: vec![],
                    },
                    ErrorCode {
                        name: "TOO_BIG".into(),
                        code: 2,
                        message: "too big".into(),
                        doc: None,
                        fields: vec![field("limit", TypeRef::Prim(Prim::I64))],
                    },
                ],
            }),
            modules: vec![],
        }],
    }
}

fn render() -> (Model, String) {
    let model = validate(&fixture(), &Identity::named("api"), None).expect("fixture validates");
    let header = render_cpp_header(&model, "api", "api.h", "api.hpp");
    (model, header)
}

fn assert_contains(header: &str, needle: &str) {
    assert!(
        header.contains(needle),
        "expected the header to contain {needle:?}\n---\n{header}"
    );
}

/// The text of the function or block that starts with `start`, up to the
/// first line that closes it at the same indentation.
fn block<'a>(header: &'a str, start: &str) -> &'a str {
    let at = header
        .find(start)
        .unwrap_or_else(|| panic!("missing {start:?}"));
    let line_start = header[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &header[line_start..at];
    let close = format!("\n{indent}}}");
    let end = header[at..]
        .find(&close)
        .unwrap_or_else(|| panic!("unterminated {start:?}"));
    &header[at..at + end + close.len()]
}

#[test]
fn library_check_embeds_every_contract_entry() {
    let (model, h) = render();
    let root = model.roots().next().expect("one root");
    let entries = contract::entries(&model, root);
    assert!(!entries.is_empty());
    for e in &entries {
        assert_contains(
            &h,
            &format!(
                "{{{:#018x}ull, {:#018x}ull, \"{}\"}},",
                e.id, e.hash, e.path
            ),
        );
    }
    assert_contains(
        &h,
        "if (std::string why = detail::contract_mismatch(api_kv_contract, detail::kv_contract); !why.empty()) {",
    );
    assert_contains(&h, "\" is missing from the library\"");
    assert_contains(&h, "\" changed since these bindings were generated\"");
    assert!(!h.contains("checksum"), "the revision 3 checksum is gone");
}

#[test]
fn vtable_starts_with_the_header_then_methods_in_order() {
    let (_, h) = render();
    let vtable = block(&h, "static const api_kv_Listener_vtable vtable = {");
    let order: Vec<usize> = [
        "static_cast<uint32_t>(sizeof(api_kv_Listener_vtable)),",
        "0,",
        "&Listener_trampolines::free_ctx,",
        "&Listener_trampolines::on_message,",
        "&Listener_trampolines::on_reset,",
        "&Listener_trampolines::pick,",
        "&Listener_trampolines::label,",
        "&Listener_trampolines::blob,",
        "&Listener_trampolines::latest,",
        "&Listener_trampolines::home,",
        "&Listener_trampolines::spare,",
    ]
    .iter()
    .map(|entry| {
        vtable
            .find(entry)
            .unwrap_or_else(|| panic!("missing {entry}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{vtable}");
}

#[test]
fn callback_returns_cross_in_every_family() {
    let (_, h) = render();
    // Strings, bytes, and buffers go back through the out slots as an
    // `alloc` run the producer adopts.
    let label = block(&h, "static void label(");
    assert_contains(
        label,
        "uint8_t** out_ptr, size_t* out_len, api_error* out_err) {",
    );
    assert_contains(
        label,
        "detail::hand_over(ret.data(), ret.size(), out_ptr, out_len);",
    );
    let blob = block(&h, "static void blob(");
    assert_contains(blob, "std::vector<uint8_t> ret = impl.blob();");
    let latest = block(&h, "static void latest(");
    assert_contains(latest, "detail::write_opt_Contact(ret_buf, ret);");
    assert_contains(
        latest,
        "detail::hand_over(ret_buf.data(), ret_buf.size(), out_ptr, out_len);",
    );
    assert_contains(&h, "inline void hand_over(");
    assert_contains(&h, "run = api_alloc(len);");
    // An object return hands over a fresh reference; `I?` may be null.
    let home = block(&h, "static api_kv_Store* home(");
    assert_contains(home, "return impl.home().clone_handle();");
    assert_contains(home, "return nullptr;");
    let spare = block(&h, "static api_kv_Store* spare(");
    assert_contains(
        spare,
        "return ret.has_value() ? ret->clone_handle() : nullptr;",
    );
    // A C-style enum converts both ways.
    assert_contains(
        &h,
        "return static_cast<api_kv_Color>(static_cast<int32_t>(impl.pick(static_cast<Color>(static_cast<int32_t>(color)))));",
    );
    assert_contains(&h, "virtual std::optional<Contact> latest() = 0;");
    assert_contains(&h, "virtual Store home() = 0;");
}

#[test]
fn only_throwing_callback_methods_report_the_domain() {
    let (_, h) = render();
    let label = block(&h, "static void label(");
    assert_contains(label, "} catch (const KvError& e) {");
    assert_contains(label, "detail::report_kv_error(e, out_err);");
    let blob = block(&h, "static void blob(");
    assert!(!blob.contains("KvError"), "{blob}");
    assert_contains(blob, "api_error_set(out_err, -4, e.what());");

    // A declared code goes back with its fields as the payload; anything
    // else is a callback failure.
    let report = block(&h, "inline void report_kv_error(");
    assert_contains(
        report,
        "case 1: api_error_set(out_err, 1, e.what()); return;",
    );
    assert_contains(
        report,
        "const auto* typed = dynamic_cast<const TooBigError*>(&e);",
    );
    assert_contains(report, "payload.write_i64(typed->limit);");
    assert_contains(
        report,
        "api_error_set_payload(out_err, payload.data(), payload.size());",
    );
    assert!(report
        .trim_end()
        .ends_with("api_error_set(out_err, -4, e.what());\n}"));
}

#[test]
fn optional_callbacks_pass_a_null_vtable() {
    let (_, h) = render();
    let maybe = block(&h, "inline void maybe_subscribe(");
    assert_contains(maybe, "(std::shared_ptr<Listener> listener) {");
    assert_contains(
        maybe,
        "const auto* listener_vtable = listener ? &detail::Listener_vtable() : nullptr;",
    );
    assert_contains(
        maybe,
        "api_kv_maybe_subscribe(static_cast<void*>(listener_ctx.release()), listener_vtable, &err);",
    );
    // A required one refuses an empty pointer before the call.
    let required = block(&h, "inline void subscribe(");
    assert_contains(
        required,
        "if (!listener) throw std::invalid_argument(\"listener: null callback interface\");",
    );
}

#[test]
fn calls_without_errors_trap_and_throwing_calls_use_the_domain() {
    let (_, h) = render();
    assert_contains(block(&h, "inline uint32_t count("), "detail::check(err);");
    assert_contains(block(&h, "inline Contact fetch("), "detail::check_kv(err);");
    assert_contains(
        block(&h, "inline void Store::save("),
        "detail::check_kv(err);",
    );
    assert_contains(&h, "class InternalError : public std::runtime_error {");
    assert_contains(
        block(&h, "inline void check(api_error& err) {"),
        "make_internal_error(err.code, error_message(err));",
    );
    // A throwing call's runtime codes are the root Error, and -5 cancels.
    let make = block(&h, "inline std::exception_ptr make_kv_error(");
    assert_contains(
        make,
        "if (code == -5) return std::make_exception_ptr(Cancelled(message));",
    );
    assert_contains(
        make,
        "if (code < 0) return std::make_exception_ptr(Error(code, message));",
    );
}

#[test]
fn each_composite_has_one_codec() {
    let (_, h) = render();
    for def in [
        "inline std::vector<Store> read_list_Store(BufferReader& r) {",
        "inline void write_list_Store(BufferWriter& w, const std::vector<Store>& v) {",
        "inline std::optional<Store> read_opt_Store(BufferReader& r) {",
        "inline std::optional<Contact> read_opt_Contact(BufferReader& r) {",
    ] {
        assert_eq!(h.matches(def).count(), 1, "{def}");
    }
    // Call sites and record codecs delegate instead of looping inline.
    assert_contains(
        block(&h, "inline Store first("),
        "detail::write_list_Store(stores_buf, stores);",
    );
    let record = block(&h, "inline Contact read_Contact(BufferReader& r) {");
    assert_contains(record, "detail::read_list_Store(r),");
    assert!(!record.contains("for ("), "{record}");
}

#[test]
fn trampolines_adopt_objects_before_anything_can_throw() {
    let (_, h) = render();
    let on_message = block(&h, "static bool on_message(");
    let adopt = on_message
        .find("Store store_arg(adopt, store);")
        .expect("adopts the object argument");
    let attempt = on_message.find("try {").expect("try");
    assert!(adopt < attempt, "{on_message}");
    let on_reset = block(&h, "static void on_reset(");
    assert_contains(on_reset, "if (alt != nullptr) alt_arg.emplace(adopt, alt);");
}

#[test]
fn identity_names_every_file_and_config_overrides_win() {
    let model =
        validate(&fixture(), &Identity::named("kv-store"), None).expect("fixture validates");
    let out = Utf8Path::new("/out");
    let files = CppGenerator.files(&model, out, &CppConfig::default());
    let paths: Vec<String> = files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(
        paths,
        [
            "/out/cpp/kv_store.h",
            "/out/cpp/kv_store.hpp",
            "/out/cpp/CMakeLists.txt",
            "/out/cpp/README.md",
        ]
    );
    let hpp = &files[1].contents;
    assert_contains(hpp, "#include \"kv_store.h\"");
    assert_contains(hpp, "namespace kv_store {");
    assert_contains(hpp, "return \"kv_store: \" + why;");

    let config = CppConfig {
        name: Some("acme::kv".into()),
        header_name: Some("acme_kv.hpp".into()),
        ..CppConfig::default()
    };
    let files = CppGenerator.files(&model, out, &config);
    assert_eq!(files[1].path, "/out/cpp/acme_kv.hpp");
    assert_contains(&files[1].contents, "namespace acme::kv {");
    assert_contains(&files[1].contents, "#include \"kv_store.h\"");
}
