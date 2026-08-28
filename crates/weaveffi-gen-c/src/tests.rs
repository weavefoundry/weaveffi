//! Tests for the C backend: rendered-header assertions over fixture APIs,
//! keyword-escaping regressions, and packaging checks.

use weaveffi_ir::ir::Api;
use weaveffi_core::resolved::ResolvedApi;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
    StructField, TypeRef,
};

use super::*;

#[test]
fn package_bundles_header_libs_and_cmake() {
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules: vec![module("calc")],
        generators: None,
        package: None,
    });
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::LinuxX64, "/s/linux-x64/libcalculator.so");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &CGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &CConfig::default(),
    )
    .expect("c supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    assert!(files
        .iter()
        .any(|f| f.path.as_str().ends_with("c/include/weaveffi.h")));
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("c/lib/darwin-arm64/libcalculator.dylib")));
    let cmake = files
        .iter()
        .find(|f| f.path.as_str().ends_with("c/CMakeLists.txt"))
        .expect("CMakeLists present");
    let FileContent::Text(txt) = &cmake.content else {
        panic!("CMakeLists is text");
    };
    assert!(
        txt.contains("IMPORTED") && txt.contains("calculator::calculator"),
        "imported target missing: {txt}"
    );
}

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
    }
}

fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }
}

fn module(name: &str) -> Module {
    Module {
        name: name.into(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        interfaces: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }
}

fn api(modules: Vec<Module>) -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules,
        generators: None,
        package: None,
    })
}

fn header(api: &ResolvedApi, prefix: &str) -> String {
    render_c_header(api, prefix, "weaveffi.yml", "weaveffi.h")
}

#[test]
fn emits_guard_and_runtime_decls() {
    let h = header(&api(vec![module("math")]), "weaveffi");
    assert!(h.contains("#ifndef WEAVEFFI_H"));
    assert!(h.contains("typedef uint64_t weaveffi_handle_t;"));
    assert!(h.contains("void weaveffi_free_string(const char* ptr);"));
}

#[test]
fn typed_handle_target_gets_forward_typedef() {
    // A typed handle's target has no declaration of its own (records are
    // value types now), so the header must forward-declare its opaque tag
    // before any prototype uses it, including across modules.
    let shared = Module {
        structs: vec![StructDef {
            name: "Token".into(),
            doc: None,
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: None,
            }],
        }],
        ..module("shared")
    };
    let main = Module {
        functions: vec![func(
            "open_typed_handle",
            vec![],
            Some(TypeRef::TypedHandle("shared.Token".into())),
        )],
        ..module("main")
    };
    let h = header(&api(vec![shared, main]), "weaveffi");
    let typedef = "typedef struct weaveffi_shared_Token weaveffi_shared_Token;";
    assert!(h.contains(typedef), "missing tag typedef, header:\n{h}");
    let use_at = h
        .find("weaveffi_shared_Token* weaveffi_main_open_typed_handle")
        .expect("prototype present");
    assert!(
        h.find(typedef).unwrap() < use_at,
        "typedef must precede its first use"
    );
}

#[test]
fn sync_function_signature() {
    let m = Module {
        functions: vec![func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )],
        ..module("math")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains("int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);"));
}

#[test]
fn custom_prefix_is_honored() {
    let m = Module {
        functions: vec![func("ping", vec![], None)],
        ..module("net")
    };
    let h = header(&api(vec![m]), "acme");
    assert!(h.contains("#ifndef ACME_H"));
    assert!(h.contains("void acme_net_ping(acme_error* out_err);"));
    assert!(h.contains("#define acme_error weaveffi_error"));
}

#[test]
fn visibility_macro_defined_and_applied_to_prototypes() {
    let m = Module {
        functions: vec![func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )],
        ..module("math")
    };
    let h = header(&api(vec![m]), "weaveffi");
    // The macro is defined behind an include guard with the three branches.
    assert!(h.contains("#ifndef WEAVEFFI_API"));
    assert!(h.contains("#      define WEAVEFFI_API __declspec(dllexport)"));
    assert!(h.contains("#      define WEAVEFFI_API __declspec(dllimport)"));
    assert!(h.contains("#    define WEAVEFFI_API __attribute__((visibility(\"default\")))"));
    // Both runtime helpers and user functions carry the export tag.
    assert!(h.contains("WEAVEFFI_API void weaveffi_free_string(const char* ptr);"));
    assert!(h.contains(
        "WEAVEFFI_API int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);"
    ));
    // Type definitions are never tagged: they declare no exported symbol.
    assert!(h.contains("typedef uint64_t weaveffi_handle_t;"));
    assert!(!h.contains("WEAVEFFI_API typedef"));
}

#[test]
fn visibility_macro_follows_custom_prefix() {
    let m = Module {
        functions: vec![func("ping", vec![], None)],
        ..module("net")
    };
    let h = header(&api(vec![m]), "acme");
    assert!(h.contains("#ifndef ACME_API"));
    assert!(h.contains("ifdef ACME_BUILD"));
    assert!(h.contains("ACME_API void acme_net_ping(acme_error* out_err);"));
    // The default-prefixed macro must not leak when a prefix is configured.
    assert!(!h.contains("WEAVEFFI_API"));
}

#[test]
fn deprecated_uses_portable_macro_not_bare_attribute() {
    let m = Module {
        functions: vec![Function {
            deprecated: Some("use bar instead".into()),
            ..func("foo", vec![], None)
        }],
        ..module("legacy")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains("#ifndef WEAVEFFI_DEPRECATED"));
    assert!(h.contains("WEAVEFFI_DEPRECATED(\"use bar instead\")"));
    // The message must travel through the macro, never a bare GCC attribute
    // (which MSVC cannot parse).
    assert!(!h.contains("__attribute__((deprecated(\"use bar instead\")))"));
}

#[test]
fn record_is_a_value_type_with_no_c_object() {
    let m = Module {
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        functions: vec![func(
            "save",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            None,
        )],
        ..module("contacts")
    };
    let h = header(&api(vec![m]), "weaveffi");
    // Records cross the ABI as serialized buffers: no opaque tag, no
    // getters, no builder machinery.
    assert!(!h.contains("typedef struct weaveffi_contacts_Contact"));
    assert!(!h.contains("Contact_get_name"));
    assert!(!h.contains("ContactBuilder"));
    // A record parameter lowers to a borrowed ptr + len view.
    assert!(h.contains(
        "void weaveffi_contacts_save(const uint8_t* contact_ptr, size_t contact_len, weaveffi_error* out_err);"
    ));
}

#[test]
fn record_return_uses_out_buffer_params() {
    let m = Module {
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        functions: vec![func(
            "load",
            vec![param("id", TypeRef::I64)],
            Some(TypeRef::Record("Contact".into())),
        )],
        ..module("contacts")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(
        h.contains(
            "const uint8_t* weaveffi_contacts_load(int64_t id, size_t* out_len, weaveffi_error* out_err);"
        ),
        "expected buffered return with out_len, header was:\n{h}"
    );
}

#[test]
fn enum_constants() {
    let m = Module {
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![EnumVariant {
                name: "Red".into(),
                value: 0,
                doc: None,
                fields: vec![],
            }],
        }],
        ..module("gfx")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains("weaveffi_gfx_Color_Red = 0"));
}

#[test]
fn iterator_emits_next_and_destroy() {
    let m = Module {
        functions: vec![func(
            "get_messages",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        )],
        ..module("events")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains(
        "weaveffi_events_GetMessagesIterator* weaveffi_events_get_messages(weaveffi_error* out_err);"
    ));
    assert!(h.contains("weaveffi_events_GetMessagesIterator_next("));
    assert!(h.contains("void weaveffi_events_GetMessagesIterator_destroy(weaveffi_events_GetMessagesIterator* iter);"));
}

#[test]
fn callback_and_listener() {
    let m = Module {
        callbacks: vec![CallbackDef {
            name: "on_message".into(),
            params: vec![param("text", TypeRef::StringUtf8)],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "messages".into(),
            event_callback: "on_message".into(),
            doc: None,
        }],
        ..module("events")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains(
        "typedef void (*weaveffi_events_on_message_fn)(const char* text, void* context);"
    ));
    assert!(h.contains("uint64_t weaveffi_events_register_messages(weaveffi_events_on_message_fn callback, void* context);"));
    assert!(h.contains("void weaveffi_events_unregister_messages(uint64_t id);"));
}

#[test]
fn interface_emits_tag_members_and_destroy() {
    use weaveffi_ir::ir::InterfaceDef;
    let m = Module {
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key/value store.".into()),
            constructors: vec![Function {
                throws: true,
                ..func("open", vec![param("path", TypeRef::StringUtf8)], None)
            }],
            methods: vec![
                func("count", vec![], Some(TypeRef::I64)),
                func(
                    "label",
                    vec![param("prefix", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                ),
            ],
            statics: vec![func("default_capacity", vec![], Some(TypeRef::I64))],
        }],
        ..module("kv")
    };
    let h = header(&api(vec![m]), "weaveffi");
    // Opaque tag plus constructor returning an owned pointer.
    assert!(h.contains("typedef struct weaveffi_kv_Store weaveffi_kv_Store;"));
    assert!(h.contains(
        "weaveffi_kv_Store* weaveffi_kv_Store_open(const char* path, weaveffi_error* out_err);"
    ));
    // Methods carry a leading const self pointer; statics do not.
    assert!(h.contains(
        "int64_t weaveffi_kv_Store_count(const weaveffi_kv_Store* self, weaveffi_error* out_err);"
    ));
    assert!(h.contains(
        "const char* weaveffi_kv_Store_label(const weaveffi_kv_Store* self, const char* prefix, weaveffi_error* out_err);"
    ));
    assert!(h.contains("int64_t weaveffi_kv_Store_default_capacity(weaveffi_error* out_err);"));
    // The destructor releases the object reference.
    assert!(h.contains("void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);"));
}

#[test]
fn interface_typed_params_and_returns_are_pointers() {
    use weaveffi_ir::ir::InterfaceDef;
    let m = Module {
        interfaces: vec![InterfaceDef {
            name: "Counter".into(),
            doc: None,
            constructors: vec![func("new", vec![], None)],
            methods: vec![func(
                "snapshot",
                vec![],
                Some(TypeRef::Interface("Counter".into())),
            )],
            statics: vec![],
        }],
        functions: vec![func(
            "read_twice",
            vec![param("counter", TypeRef::Interface("Counter".into()))],
            Some(TypeRef::I64),
        )],
        ..module("counters")
    };
    let h = header(&api(vec![m]), "weaveffi");
    // A method returning the interface hands back an owned pointer.
    assert!(h.contains(
        "weaveffi_counters_Counter* weaveffi_counters_Counter_snapshot(const weaveffi_counters_Counter* self, weaveffi_error* out_err);"
    ));
    // A free function borrows the interface as a const pointer.
    assert!(h.contains(
        "int64_t weaveffi_counters_read_twice(const weaveffi_counters_Counter* counter, weaveffi_error* out_err);"
    ));
}

#[test]
fn error_domain_emits_code_enum() {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
    let m = Module {
        errors: Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: None,
                    fields: vec![],
                },
                ErrorCode {
                    name: "Expired".into(),
                    code: 1002,
                    message: "entry expired".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        functions: vec![Function {
            throws: true,
            ..func(
                "get",
                vec![param("key", TypeRef::StringUtf8)],
                Some(TypeRef::I64),
            )
        }],
        ..module("kv")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains("weaveffi_kv_KvError_KeyNotFound = 1001"));
    assert!(h.contains("weaveffi_kv_KvError_Expired = 1002"));
}

#[test]
fn async_emits_callback_typedef_and_launcher() {
    let m = Module {
        functions: vec![Function {
            r#async: true,
            cancellable: true,
            ..func(
                "fetch",
                vec![param("id", TypeRef::I64)],
                Some(TypeRef::StringUtf8),
            )
        }],
        ..module("net")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(h.contains("typedef void (*weaveffi_net_fetch_callback)(void* context, weaveffi_error* err, const char* result);"));
    assert!(h.contains("weaveffi_net_fetch_async("));
    assert!(h.contains("weaveffi_cancel_token* cancel_token"));
}

#[test]
fn output_files_lists_header_and_source() {
    let tmp = std::env::temp_dir().join("weaveffi_c_outfiles");
    let out_dir = Utf8Path::from_path(&tmp).unwrap();
    let files = CGenerator.output_files(&api(vec![module("m")]), out_dir, &CConfig::default());
    assert!(files.iter().any(|f| f.ends_with("c/weaveffi.h")));
    assert!(files.iter().any(|f| f.ends_with("c/weaveffi.c")));
}

#[test]
fn reserved_c_keyword_params_are_escaped_in_prototypes() {
    // A parameter named after a C keyword previously landed verbatim in the
    // prototype (`int32_t register`), which no C compiler accepts. The shared
    // escape appends a trailing underscore; non-reserved names pass through.
    let m = Module {
        functions: vec![func(
            "configure",
            vec![
                param("register", TypeRef::I32),
                param("volatile", TypeRef::StringUtf8),
                param("value", TypeRef::I64),
            ],
            None,
        )],
        ..module("cfg")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(
        h.contains(
            "void weaveffi_cfg_configure(int32_t register_, const char* volatile_, int64_t value, weaveffi_error* out_err);"
        ),
        "keyword params must gain a trailing underscore, header was:\n{h}"
    );
}

#[test]
fn reserved_c_keyword_params_are_escaped_in_callback_typedefs() {
    let m = Module {
        callbacks: vec![CallbackDef {
            name: "on_tick".into(),
            params: vec![param("restrict", TypeRef::StringUtf8)],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "ticks".into(),
            event_callback: "on_tick".into(),
            doc: None,
        }],
        ..module("events")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(
        h.contains(
            "typedef void (*weaveffi_events_on_tick_fn)(const char* restrict_, void* context);"
        ),
        "callback keyword params must be escaped, header was:\n{h}"
    );
}

#[test]
fn reserved_c_keyword_params_are_escaped_in_async_and_methods() {
    use weaveffi_ir::ir::InterfaceDef;
    let m = Module {
        functions: vec![Function {
            r#async: true,
            ..func("fetch", vec![param("switch", TypeRef::I32)], None)
        }],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![func("new", vec![], None)],
            methods: vec![func("put", vec![param("union", TypeRef::I64)], None)],
            statics: vec![],
        }],
        ..module("kv")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(
        h.contains("int32_t switch_,"),
        "async launcher keyword params must be escaped, header was:\n{h}"
    );
    assert!(
        h.contains("int64_t union_,"),
        "method keyword params must be escaped, header was:\n{h}"
    );
}

#[test]
fn derived_buffer_slot_names_pass_through_unescaped() {
    // A bytes or buffered parameter named after a keyword lowers to derived
    // `{name}_ptr` / `{name}_len` slots, which are never keywords themselves
    // and must stay untouched.
    let m = Module {
        functions: vec![func("write", vec![param("int", TypeRef::Bytes)], None)],
        ..module("io")
    };
    let h = header(&api(vec![m]), "weaveffi");
    assert!(
        h.contains("const uint8_t* int_ptr, size_t int_len,"),
        "derived slot names must pass through, header was:\n{h}"
    );
}
