//! Unit tests: render a small API that exercises every C ABI revision 3
//! shape and assert the header carries the key pieces of each contract.

use camino::Utf8Path;
use weaveffi_model::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function,
    InterfaceDef, Module, Param, StructDef, StructField, TypeRef,
};
use weaveffi_model::model::{BindingModel, CallShape};
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;
use weaveffi_model::validate::validate_api;

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

/// One module covering: an interface with a constructor, methods, a static,
/// an iterator of objects, and an async object result; a function taking and
/// returning `Interface?`; a record with `Interface`, `[Interface]`, and
/// `Interface?` fields; a callback interface with string, i32, record,
/// object, nullable object, and enum parameters and `bool`, void, and enum
/// returns; and a function taking that callback interface.
fn fixture() -> Api {
    let store = InterfaceDef {
        name: "Store".into(),
        doc: Some("A reference-counted key-value store.".into()),
        deprecated: None,
        constructors: vec![func("new", vec![param("path", TypeRef::StringUtf8)], None)],
        methods: vec![
            func(
                "get",
                vec![param("key", TypeRef::StringUtf8)],
                Some(optional(TypeRef::StringUtf8)),
            ),
            func("sibling", vec![], Some(optional(named("Store")))),
            func(
                "scan",
                vec![],
                Some(TypeRef::Iterator(Box::new(named("Store")))),
            ),
            Function {
                throws: true,
                ..func("save", vec![param("contact", named("Contact"))], None)
            },
            Function {
                r#async: true,
                ..func("fetch", vec![], Some(named("Store")))
            },
        ],
        statics: vec![func("open_default", vec![], Some(named("Store")))],
    };
    let listener = CallbackInterfaceDef {
        name: "Listener".into(),
        doc: Some("Receives store events.".into()),
        deprecated: None,
        methods: vec![
            func(
                "on_message",
                vec![
                    param("text", TypeRef::StringUtf8),
                    param("weight", TypeRef::I32),
                    param("contact", named("Contact")),
                    param("store", named("Store")),
                ],
                Some(TypeRef::Bool),
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
        ],
    };
    Api {
        version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
        modules: vec![Module {
            name: "kv".into(),
            doc: None,
            functions: vec![
                func(
                    "lookup",
                    vec![param("store", optional(named("Store")))],
                    Some(optional(named("Store"))),
                ),
                func(
                    "subscribe",
                    vec![param("listener", named("Listener"))],
                    None,
                ),
                func(
                    "all_stores",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(named("Store")))),
                ),
                func(
                    "first",
                    vec![param("stores", TypeRef::List(Box::new(named("Store"))))],
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
                    field("name", TypeRef::StringUtf8),
                    field("store", named("Store")),
                    field("mirrors", TypeRef::List(Box::new(named("Store")))),
                    field("primary", optional(named("Store"))),
                ],
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                deprecated: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![ErrorCode {
                    name: "NOT_FOUND".into(),
                    code: 1,
                    message: "not found".into(),
                    doc: None,
                    fields: vec![],
                }],
            }),
            modules: vec![],
        }],
    }
}

/// Render the wrapper header for `api` with the default names (namespace and
/// files named after the identity, `api` for an API built in memory).
fn render_api(api: &ResolvedApi) -> (BindingModel, String) {
    let model = BindingModel::build(api);
    let id = api.identity();
    let header = render_cpp_header(
        &model,
        id,
        &id.prefix,
        &format!("{}.h", id.library),
        "kv.yml",
        &format!("{}.hpp", id.library),
    );
    (model, header)
}

fn render() -> (BindingModel, String) {
    render_api(&validate_api(fixture(), None).expect("fixture validates"))
}

fn assert_contains(header: &str, needle: &str) {
    assert!(
        header.contains(needle),
        "expected header to contain {needle:?}\n---\n{header}"
    );
}

fn assert_before(header: &str, first: &str, second: &str) {
    let a = header
        .find(first)
        .unwrap_or_else(|| panic!("missing {first:?}"));
    let b = header
        .find(second)
        .unwrap_or_else(|| panic!("missing {second:?}"));
    assert!(a < b, "{first:?} must precede {second:?}");
}

#[test]
fn interface_wrapper_is_reference_counted_raii() {
    let (_, h) = render();
    assert_contains(&h, "class Store {");
    assert_contains(&h, "api_kv_Store* handle_;");
    assert_contains(&h, "using raw_type = api_kv_Store;");
    assert_contains(
        &h,
        "explicit Store(adopt_t, api_kv_Store* h) noexcept : handle_(h) {}",
    );
    assert_contains(&h, "if (handle_) api_kv_Store_destroy(handle_);");
    assert_contains(
        &h,
        "Store(const Store& other) : handle_(api_kv_Store_clone(other.handle_)) {}",
    );
    assert_contains(
        &h,
        "Store(Store&& other) noexcept : handle_(other.handle_) {",
    );
    assert_contains(&h, "const api_kv_Store* handle() const { return handle_; }");
    assert_contains(
        &h,
        "api_kv_Store* clone_handle() const { return api_kv_Store_clone(handle_); }",
    );
    // Constructor adopts the returned reference; methods borrow `handle_`.
    assert_contains(&h, "    explicit Store(std::string_view path);");
    assert_contains(
        &h,
        "inline Store::Store(std::string_view path) : handle_(nullptr) {\n    check_library();",
    );
    assert_contains(
        &h,
        "auto result = api_kv_Store_new(detail::utf8(path), path.size(), &err);",
    );
    assert_contains(&h, "handle_ = result;");
    assert_contains(
        &h,
        "inline std::optional<std::string> Store::get(std::string_view key) const {\n    size_t out_len = 0;",
    );
    assert_contains(
        &h,
        "api_kv_Store_get(handle_, detail::utf8(key), key.size(), &out_len, &err);",
    );
    assert_contains(&h, "static Store open_default();");
    assert_contains(
        &h,
        "inline Store Store::open_default() {\n    check_library();",
    );
    // The throwing method routes through the typed domain check.
    assert_contains(
        &h,
        "inline void Store::save(const Contact& contact) const {",
    );
    assert_contains(&h, "detail::check_kv(err);");
    // Async object results are adopted.
    assert_contains(&h, "inline std::future<Store> Store::fetch() const {");
    assert_contains(&h, "p->set_value(Store(adopt, result));");
}

#[test]
fn nullable_objects_map_to_optional() {
    let (_, h) = render();
    assert_contains(
        &h,
        "inline std::optional<Store> lookup(const std::optional<Store>& store) {\n    check_library();",
    );
    assert_contains(
        &h,
        "api_kv_lookup(store.has_value() ? store->handle() : nullptr, &err);",
    );
    assert_contains(&h, "if (!result) return std::nullopt;");
    assert_contains(&h, "return Store(adopt, result);");
    assert_contains(&h, "std::optional<Store> sibling() const;");
}

#[test]
fn records_hold_objects_as_cloned_tokens() {
    let (_, h) = render();
    assert_contains(&h, "struct Contact {");
    assert_contains(&h, "    Store store;");
    assert_contains(&h, "    std::vector<Store> mirrors;");
    assert_contains(&h, "    std::optional<Store> primary;");
    // The wrapper class is complete before the record that holds it, and
    // the member bodies come after the record's codec.
    assert_before(&h, "class Store {", "struct Contact {");
    assert_before(&h, "inline Contact read_Contact(", "inline Store::Store(");
    // Writing mints a fresh reference; reading adopts the token.
    assert_contains(
        &h,
        "w.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>(v.store.clone_handle())));",
    );
    assert_contains(
        &h,
        "w.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>(item0.clone_handle())));",
    );
    assert_contains(
        &h,
        "Store f_store = Store(adopt, reinterpret_cast<Store::raw_type*>(static_cast<uintptr_t>(r.read_u64())));",
    );
    assert_contains(
        &h,
        "return Contact{std::move(f_name), std::move(f_store), std::move(f_mirrors), std::move(f_primary)};",
    );
    // A list of objects as a parameter is encoded the same way.
    assert_contains(&h, "inline Store first(const std::vector<Store>& stores) {");
    assert_contains(&h, "stores_buf.write_len(stores.size());");
}

#[test]
fn iterators_over_objects_adopt_each_element() {
    let (model, h) = render();
    let scan = &model.modules[0].interfaces[0].methods[2];
    let CallShape::Iterator(it) = &scan.shape else {
        panic!("scan is an iterator");
    };
    assert_contains(&h, "class StoreScanIterator;");
    assert_contains(&h, "class StoreScanIterator {");
    assert_contains(&h, &format!("{}* handle_;", it.iter_tag));
    assert_contains(&h, &format!("if (handle_) {}(handle_);", it.destroy_symbol));
    assert_contains(&h, "std::optional<Store> next() {");
    assert_contains(&h, "return Store(adopt, item);");
    assert_contains(&h, "StoreScanIterator scan() const;");
    assert_contains(&h, "inline StoreScanIterator Store::scan() const {");
    assert_contains(&h, "class AllStoresIterator {");
    assert_contains(&h, "inline AllStoresIterator all_stores() {");
    assert_before(
        &h,
        "class StoreScanIterator {",
        "inline StoreScanIterator Store::scan()",
    );
}

#[test]
fn callback_interface_renders_abstract_class_vtable_and_trampolines() {
    let (_, h) = render();
    assert_contains(&h, "class Listener {");
    assert_contains(&h, "virtual ~Listener() = default;");
    assert_contains(
        &h,
        "virtual bool on_message(std::string_view text, int32_t weight, const Contact& contact, Store store) = 0;",
    );
    assert_contains(&h, "virtual void on_reset(std::optional<Store> alt) = 0;");
    assert_contains(&h, "virtual Color pick(Color color) = 0;");

    // Trampolines carry the exact vtable entry signatures.
    assert_contains(&h, "struct Listener_trampolines {");
    assert_contains(
        &h,
        "static bool on_message(void* ctx, const uint8_t* text_ptr, size_t text_len, int32_t weight, const uint8_t* contact_ptr, size_t contact_len, api_kv_Store* store, api_error* out_err) {",
    );
    assert_contains(
        &h,
        "static void on_reset(void* ctx, api_kv_Store* alt, api_error* out_err) {",
    );
    assert_contains(
        &h,
        "static api_kv_Color pick(void* ctx, api_kv_Color color, api_error* out_err) {",
    );
    // Objects are adopted before anything that can throw; buffers and
    // strings are decoded (borrowed, never freed).
    assert_contains(&h, "Store store_val(adopt, store);");
    assert_before(
        &h,
        "Store store_val(adopt, store);",
        "Listener& impl = **static_cast<std::shared_ptr<Listener>*>(ctx);",
    );
    assert_contains(&h, "std::optional<Store> alt_val;");
    assert_contains(&h, "if (alt) alt_val.emplace(adopt, alt);");
    assert_contains(
        &h,
        "std::string_view text_val = detail::borrow_string(text_ptr, text_len);",
    );
    assert_contains(
        &h,
        "detail::BufferReader contact_r(contact_ptr, contact_len);",
    );
    assert_contains(&h, "Contact contact_val = detail::read_Contact(contact_r);");
    assert_contains(
        &h,
        "return impl.on_message(text_val, weight, contact_val, std::move(store_val));",
    );
    assert_contains(&h, "impl.on_reset(std::move(alt_val));");
    assert_contains(
        &h,
        "return static_cast<api_kv_Color>(static_cast<int32_t>(impl.pick(static_cast<Color>(static_cast<int32_t>(color)))));",
    );
    // Failure path: foreign error code -4, default return, no unwinding.
    assert_contains(&h, "} catch (const std::exception& e) {");
    assert_contains(&h, "api_error_set(out_err, -4, e.what());");
    assert_contains(&h, "return bool{};");
    assert_contains(&h, "return api_kv_Color{};");
    assert!(!h.contains("api_free_bytes(const_cast<uint8_t*>(contact_ptr)"));

    // Exactly one process-wide vtable, methods in declaration order then free.
    assert_eq!(
        h.matches("static const api_kv_Listener_vtable vtable = {")
            .count(),
        1
    );
    assert_contains(
        &h,
        "inline const api_kv_Listener_vtable& Listener_vtable() {",
    );
    let vtable_start = h
        .find("static const api_kv_Listener_vtable vtable = {")
        .unwrap();
    let vtable = &h[vtable_start..];
    let order: Vec<usize> = [
        "&Listener_trampolines::on_message,",
        "&Listener_trampolines::on_reset,",
        "&Listener_trampolines::pick,",
        "&Listener_trampolines::free_ctx,",
    ]
    .iter()
    .map(|entry| {
        vtable
            .find(entry)
            .unwrap_or_else(|| panic!("missing {entry}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert_contains(&h, "delete static_cast<std::shared_ptr<Listener>*>(ctx);");
}

#[test]
fn passing_a_callback_interface_boxes_the_shared_ptr() {
    let (_, h) = render();
    assert_contains(
        &h,
        "inline void subscribe(std::shared_ptr<Listener> listener) {",
    );
    assert_contains(
        &h,
        "if (!listener) throw std::invalid_argument(\"listener: null callback interface\");",
    );
    assert_contains(
        &h,
        "auto listener_ctx = std::make_unique<std::shared_ptr<Listener>>(std::move(listener));",
    );
    assert_contains(
        &h,
        "api_kv_subscribe(static_cast<void*>(listener_ctx.release()), &detail::Listener_vtable(), &err);",
    );
}

#[test]
fn runtime_surface_matches_abi_revision_3() {
    let (model, h) = render();
    // The C ABI comes from the shipped C header, never re-declared.
    assert_contains(&h, "#include \"api.h\"");
    assert!(
        !h.contains("extern \"C\""),
        "C declarations are not inlined"
    );
    assert_contains(&h, "namespace api {");
    // The library check covers the ABI revision and every root checksum.
    let sum = model.modules[0].checksum.expect("roots carry a checksum");
    assert_contains(&h, "inline void check_library() {");
    assert_contains(&h, "if (abi != API_ABI_VERSION) {");
    assert_contains(&h, &format!("if (api_kv_checksum() != {sum:#018x}ull) {{"));
    assert_contains(&h, "module 'kv' does not match the linked library");
    assert_contains(&h, "if (!failure.empty()) throw LoadError(failure);");
    // Errors: the base, cancellation, and load failures.
    assert_contains(&h, "class Error : public std::runtime_error {");
    assert_contains(&h, "class Cancelled : public Error {");
    assert_contains(&h, "class LoadError : public Error {");
    assert_contains(
        &h,
        "if (code == -5) return std::make_exception_ptr(Cancelled(message));",
    );
    // Negative codes, including -4 and -5, route to the runtime exceptions.
    assert_contains(&h, "if (code < 0) return make_error(code, msg);");
    // Returned strings are copied by length and released with free_bytes.
    assert_contains(
        &h,
        "~Release() { api_free_bytes(const_cast<uint8_t*>(ptr), len); }",
    );
    assert_contains(
        &h,
        "return std::string(reinterpret_cast<const char*>(ptr), len);",
    );
    assert_contains(&h, "class CancelToken {");
    assert_contains(&h, "CancelToken() : handle_(api_cancel_token_create()) {");
    for legacy in [
        "free_string",
        "c_str()",
        "WeaveFFI",
        "weaveffi",
        "check_abi_version",
        "arena",
        "handle_t",
        "std::function",
    ] {
        let body = h.split_once("#pragma once").expect("prelude").1;
        assert!(!body.contains(legacy), "legacy token {legacy:?} in header");
    }
}

#[test]
fn identity_names_every_file_and_config_overrides_win() {
    let api = validate_api(fixture(), None)
        .expect("fixture validates")
        .with_identity(Identity::named("kv-store"));
    let model = BindingModel::build(&api);
    let out = Utf8Path::new("/out");
    let files = CppGenerator.files(&api, &model, out, &CppConfig::default());
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
    assert_contains(hpp, "kv_store: C ABI revision mismatch");
    let cmake = &files[2].contents;
    assert_contains(cmake, "add_library(kv_store_cpp INTERFACE)");
    assert_contains(cmake, "add_library(kv_store::cpp ALIAS kv_store_cpp)");
    assert_contains(
        cmake,
        "set(KV_STORE_LIBRARY \"$ENV{KV_STORE_LIBRARY}\" CACHE FILEPATH",
    );
    assert_contains(
        cmake,
        "target_link_libraries(kv_store_cpp INTERFACE kv_store)",
    );

    let config = CppConfig {
        namespace: Some("acme::kv".into()),
        header_name: Some("acme_kv.hpp".into()),
        ..CppConfig::default()
    };
    let files = CppGenerator.files(&api, &model, out, &config);
    assert_eq!(files[1].path, "/out/cpp/acme_kv.hpp");
    assert_contains(&files[1].contents, "namespace acme::kv {");
    assert_contains(&files[1].contents, "#include \"kv_store.h\"");
}

#[test]
fn constructors_and_adoption_are_explicit() {
    let (_, h) = render();
    // No implicit conversion from arguments or raw pointers into an object.
    assert!(!h.contains("\n    Store(std::string_view path);"));
    assert!(!h.contains("Store(api_kv_Store* h)"));
    assert_contains(&h, "inline constexpr adopt_t adopt{};");
}

#[test]
fn extended_shapes_render() {
    let api = Api {
        version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
        modules: vec![Module {
            name: "ext".into(),
            doc: None,
            functions: vec![
                func(
                    "maybe_stores",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(optional(named("Store"))))),
                ),
                func(
                    "roundtrip",
                    vec![
                        param("blob", TypeRef::Bytes),
                        param(
                            "m",
                            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(named("Store"))),
                        ),
                        param("shape", named("Shape")),
                    ],
                    Some(named("Shape")),
                ),
                Function {
                    r#async: true,
                    cancellable: true,
                    throws: true,
                    ..func("slow", vec![param("n", TypeRef::I64)], Some(TypeRef::Bytes))
                },
                Function {
                    r#async: true,
                    ..func("fire", vec![], None)
                },
                Function {
                    throws: true,
                    ..func("fail", vec![], Some(TypeRef::F64))
                },
                func(
                    "watch",
                    vec![param("w", named("Watcher"))],
                    Some(TypeRef::Bool),
                ),
            ],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: None,
                deprecated: None,
                constructors: vec![],
                methods: vec![func("size", vec![], Some(TypeRef::U64))],
                statics: vec![],
            }],
            callback_interfaces: vec![CallbackInterfaceDef {
                name: "Watcher".into(),
                doc: None,
                deprecated: None,
                methods: vec![
                    func(
                        "on_data",
                        vec![
                            param("blob", TypeRef::Bytes),
                            param("names", TypeRef::List(Box::new(TypeRef::StringUtf8))),
                            param("label", optional(TypeRef::StringUtf8)),
                            param("shape", named("Shape")),
                            param("ratio", TypeRef::F64),
                        ],
                        Some(TypeRef::I64),
                    ),
                    func(
                        "on_flag",
                        vec![param("flag", TypeRef::Bool)],
                        Some(TypeRef::Bool),
                    ),
                ],
            }],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Shape".into(),
                doc: None,
                deprecated: None,
                variants: vec![
                    EnumVariant {
                        name: "Dot".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Boxed".into(),
                        value: 1,
                        doc: None,
                        fields: vec![
                            field("store", named("Store")),
                            field("tags", TypeRef::List(Box::new(TypeRef::StringUtf8))),
                        ],
                    },
                ],
            }],
            errors: Some(ErrorDomain {
                name: "ExtError".into(),
                codes: vec![ErrorCode {
                    name: "TOO_BIG".into(),
                    code: 1,
                    message: "too big".into(),
                    doc: None,
                    fields: vec![
                        field("limit", TypeRef::I64),
                        field("culprit", optional(named("Store"))),
                    ],
                }],
            }),
            modules: vec![],
        }],
    };
    let (_, h) = render_api(&validate_api(api, None).expect("fixture validates"));

    // Nullable iterator elements, object payloads in rich enums and error
    // fields, maps of objects, and a cancellable async bytes result.
    assert_contains(&h, "std::optional<std::optional<Store>> next() {");
    assert_contains(&h, "if (item) value.emplace(adopt, item);");
    assert_contains(
        &h,
        "return Shape{Shape::Boxed{std::move(f_store), std::move(f_tags)}};",
    );
    assert_contains(
        &h,
        "w.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>(p.store.clone_handle())));",
    );
    assert_contains(
        &h,
        "TooBigError(const std::string& msg, int64_t limit, std::optional<Store> culprit)",
    );
    assert_contains(
        &h,
        "m_buf.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>(kv0.second.clone_handle())));",
    );
    assert_contains(
        &h,
        "inline std::future<std::vector<uint8_t>> slow(int64_t n, const CancelToken& cancel_token = CancelToken::none()) {",
    );
    assert_contains(
        &h,
        "api_ext_slow(n, cancel_token.handle(), [](void* context, api_error* err, const uint8_t* result_ptr, size_t result_len) {",
    );
    assert_contains(
        &h,
        "std::unique_ptr<std::promise<std::vector<uint8_t>>> p(static_cast<std::promise<std::vector<uint8_t>>*>(context));",
    );
    assert_contains(
        &h,
        "p->set_value(detail::take_bytes(result_ptr, result_len));",
    );
    assert_contains(&h, "}, static_cast<void*>(promise.release()));");
    assert_contains(
        &h,
        "p->set_exception(detail::make_ext_error(err->code, msg, err->payload_ptr, err->payload_len));",
    );
    // Callback parameters in the buffered family decode into owned values.
    assert_contains(
        &h,
        "virtual int64_t on_data(const std::vector<uint8_t>& blob, const std::vector<std::string>& names, const std::optional<std::string>& label, const Shape& shape, double ratio) = 0;",
    );
    assert_contains(
        &h,
        "std::vector<uint8_t> blob_val = detail::borrow_bytes(blob_ptr, blob_len);",
    );
    assert_contains(&h, "Shape shape_val = detail::read_Shape(shape_r);");
    assert_contains(&h, "return int64_t{};");
}

#[test]
fn rendering_is_deterministic() {
    let (_, a) = render();
    let (_, b) = render();
    assert_eq!(a, b);
}
