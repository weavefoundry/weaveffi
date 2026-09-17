//! Unit tests: render a small API that exercises every ABI revision 2 shape
//! and assert the header carries the key pieces of each contract.

use weaveffi_core::model::{BindingModel, CallShape};
use weaveffi_core::validate::validate_api;
use weaveffi_ir::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function,
    InterfaceDef, Module, Param, StructDef, StructField, TypeRef,
};

use crate::render_cpp_header;

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
        version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
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

fn render() -> (BindingModel, String) {
    let api = validate_api(fixture(), None).expect("fixture validates");
    let model = BindingModel::build(&api, "weaveffi");
    let header = render_cpp_header(&model, "weaveffi", "kv.yml", "weaveffi.hpp");
    (model, header)
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
    assert_contains(&h, "weaveffi_kv_Store* handle_;");
    assert_contains(&h, "explicit Store(weaveffi_kv_Store* h) : handle_(h) {}");
    assert_contains(&h, "if (handle_) weaveffi_kv_Store_destroy(handle_);");
    assert_contains(
        &h,
        "Store(const Store& other) : handle_(weaveffi_kv_Store_clone(other.handle_)) {}",
    );
    assert_contains(
        &h,
        "Store(Store&& other) noexcept : handle_(other.handle_) {",
    );
    assert_contains(
        &h,
        "const weaveffi_kv_Store* handle() const { return handle_; }",
    );
    assert_contains(
        &h,
        "weaveffi_kv_Store* clone_handle() const { return weaveffi_kv_Store_clone(handle_); }",
    );
    // Constructor adopts the returned reference; methods borrow `handle_`.
    assert_contains(&h, "    Store(const std::string& path);");
    assert_contains(
        &h,
        "inline Store::Store(const std::string& path) : handle_(nullptr) {",
    );
    assert_contains(
        &h,
        "auto result = weaveffi_kv_Store_new(path.c_str(), &err);",
    );
    assert_contains(&h, "handle_ = result;");
    assert_contains(
        &h,
        "inline std::optional<std::string> Store::get(const std::string& key) const {",
    );
    assert_contains(
        &h,
        "weaveffi_kv_Store_get(handle_, key.c_str(), &out_len, &err);",
    );
    assert_contains(&h, "static Store open_default();");
    assert_contains(&h, "inline Store Store::open_default() {");
    // The throwing method routes through the typed domain check.
    assert_contains(
        &h,
        "inline void Store::save(const Contact& contact) const {",
    );
    assert_contains(&h, "detail::check_kv(err);");
    // Async object results are adopted.
    assert_contains(&h, "inline std::future<Store> Store::fetch() const {");
    assert_contains(&h, "p->set_value(Store(result));");
}

#[test]
fn nullable_objects_map_to_optional() {
    let (_, h) = render();
    assert_contains(
        &h,
        "inline std::optional<Store> lookup(const std::optional<Store>& store) {",
    );
    assert_contains(
        &h,
        "weaveffi_kv_lookup(store.has_value() ? store->handle() : nullptr, &err);",
    );
    assert_contains(&h, "if (!result) return std::nullopt;");
    assert_contains(&h, "return Store(result);");
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
        "Store f_store = Store(reinterpret_cast<weaveffi_kv_Store*>(static_cast<uintptr_t>(r.read_u64())));",
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
    assert_contains(&h, "return Store(item);");
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
        "virtual bool on_message(const std::string& text, int32_t weight, const Contact& contact, Store store) = 0;",
    );
    assert_contains(&h, "virtual void on_reset(std::optional<Store> alt) = 0;");
    assert_contains(&h, "virtual Color pick(Color color) = 0;");

    // Trampolines carry the exact vtable entry signatures.
    assert_contains(&h, "struct Listener_trampolines {");
    assert_contains(
        &h,
        "static bool on_message(void* ctx, const char* text, int32_t weight, const uint8_t* contact_ptr, size_t contact_len, weaveffi_kv_Store* store, weaveffi_error* out_err) {",
    );
    assert_contains(
        &h,
        "static void on_reset(void* ctx, weaveffi_kv_Store* alt, weaveffi_error* out_err) {",
    );
    assert_contains(
        &h,
        "static weaveffi_kv_Color pick(void* ctx, weaveffi_kv_Color color, weaveffi_error* out_err) {",
    );
    // Objects are adopted before anything that can throw; buffers and
    // strings are decoded (borrowed, never freed).
    assert_contains(&h, "Store store_val(store);");
    assert_before(
        &h,
        "Store store_val(store);",
        "Listener& impl = **static_cast<std::shared_ptr<Listener>*>(ctx);",
    );
    assert_contains(&h, "std::optional<Store> alt_val;");
    assert_contains(&h, "if (alt) alt_val.emplace(alt);");
    assert_contains(&h, "std::string text_val(text ? text : \"\");");
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
        "return static_cast<weaveffi_kv_Color>(static_cast<int32_t>(impl.pick(static_cast<Color>(static_cast<int32_t>(color)))));",
    );
    // Failure path: foreign error code -4, default return, no unwinding.
    assert_contains(&h, "} catch (const std::exception& e) {");
    assert_contains(&h, "weaveffi_error_set(out_err, -4, e.what());");
    assert_contains(&h, "return bool{};");
    assert_contains(&h, "return weaveffi_kv_Color{};");
    assert!(!h.contains("weaveffi_free_bytes(const_cast<uint8_t*>(contact_ptr)"));

    // Exactly one process-wide vtable, methods in declaration order then free.
    assert_eq!(
        h.matches("static const weaveffi_kv_Listener_vtable vtable = {")
            .count(),
        1
    );
    assert_contains(
        &h,
        "inline const weaveffi_kv_Listener_vtable& Listener_vtable() {",
    );
    let vtable_start = h
        .find("static const weaveffi_kv_Listener_vtable vtable = {")
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
        "auto* listener_ctx = new std::shared_ptr<Listener>(std::move(listener));",
    );
    assert_contains(
        &h,
        "weaveffi_kv_subscribe(static_cast<void*>(listener_ctx), &detail::Listener_vtable(), &err);",
    );
}

#[test]
fn runtime_surface_matches_abi_revision_2() {
    let (_, h) = render();
    assert_contains(&h, "#define WEAVEFFI_ABI_VERSION 2u");
    assert_contains(&h, "inline void check_abi_version() {");
    assert_contains(
        &h,
        "WEAVEFFI_API void weaveffi_error_set(weaveffi_error* err, int32_t code, const char* message);",
    );
    assert_contains(
        &h,
        "WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_clone(const weaveffi_kv_Store* self);",
    );
    for legacy in [
        "arena",
        "handle_t",
        "register_",
        "unregister_",
        "std::function",
    ] {
        assert!(!h.contains(legacy), "legacy token {legacy:?} in header");
    }
    // Negative codes, including -4, route to the generic error.
    assert_contains(
        &h,
        "if (code < 0) return std::make_exception_ptr(WeaveFFIError(code, msg));",
    );
}

#[test]
fn extended_shapes_render() {
    let api = Api {
        version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
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
    let api = validate_api(api, None).expect("fixture validates");
    let model = BindingModel::build(&api, "weaveffi");
    let h = render_cpp_header(&model, "weaveffi", "ext.yml", "weaveffi.hpp");

    // Nullable iterator elements, object payloads in rich enums and error
    // fields, maps of objects, and a cancellable async bytes result.
    assert_contains(&h, "std::optional<std::optional<Store>> next() {");
    assert_contains(&h, "if (item) value.emplace(item);");
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
        "inline std::future<std::vector<uint8_t>> slow(int64_t n, weaveffi_cancel_token* cancel_token) {",
    );
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
        "std::vector<uint8_t> blob_val(blob_ptr, blob_ptr + blob_len);",
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
