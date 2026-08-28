//! Unit tests: golden-content assertions over the JS loader, the
//! TypeScript declarations, the README, and the package manifest.

use weaveffi_ir::ir::Api;
use weaveffi_core::resolved::ResolvedApi;
use super::*;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

use crate::dts::render_wasm_dts;
use crate::entities::render_wasm_js_stub;
use crate::package::render_wasm_readme;
use crate::types::{ts_type_for, type_display, wasm_type, wasm_type_note};

fn empty_api() -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules: vec![],
        generators: None,
        package: None,
    })
}

fn make_api(modules: Vec<Module>) -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules,
        generators: None,
        package: None,
    })
}

/// Test-only shim: build the model (the driver's job in production) and
/// render the JS stub with the historical argument order.
fn js_stub_for(
    api: &ResolvedApi,
    module_name: &str,
    prefix: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let model = BindingModel::build(api, prefix);
    render_wasm_js_stub(
        api,
        &model,
        module_name,
        prefix,
        input_basename,
        filename,
        emscripten,
    )
}

/// Test-only shim mirroring [`js_stub_for`] for the `.d.ts` renderer.
fn dts_for(
    api: &ResolvedApi,
    module_name: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let model = BindingModel::build(api, "weaveffi");
    render_wasm_dts(
        api,
        &model,
        module_name,
        input_basename,
        filename,
        emscripten,
    )
}

/// Test-only shim mirroring [`js_stub_for`] for the README renderer.
fn readme_for(api: &ResolvedApi, prefix: &str, input_basename: &str, emscripten: bool) -> String {
    let model = BindingModel::build(api, prefix);
    render_wasm_readme(api, &model, prefix, input_basename, emscripten)
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn member(
    name: &str,
    params: Vec<Param>,
    returns: Option<TypeRef>,
    throws: bool,
    is_async: bool,
) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws,
        r#async: is_async,
        cancellable: is_async,
        deprecated: None,
        since: None,
    }
}

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
    }
}

fn str_param(name: &str) -> Param {
    param(name, TypeRef::StringUtf8)
}

fn module(name: &str) -> Module {
    Module {
        name: name.into(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }
}

fn sample_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![Function {
            name: "add".into(),
            params: vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: Some("Add two numbers".into()),
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Point".into(),
            doc: Some("A 2D point".into()),
            fields: vec![field("x", TypeRef::F64), field("y", TypeRef::F64)],
        }],
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: Some("Primary colors".into()),
            variants: vec![
                EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Blue".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        ..module("math")
    }])
}

/// An API with a callback + listener, delivered synchronously through a
/// long-lived function-table trampoline in the standard loader (and
/// stubbed in Emscripten mode).
fn listener_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member("send", vec![str_param("text")], None, false, false)],
        callbacks: vec![weaveffi_ir::ir::CallbackDef {
            name: "OnMessage".into(),
            params: vec![str_param("message")],
            doc: None,
        }],
        listeners: vec![weaveffi_ir::ir::ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        ..module("events")
    }])
}

#[test]
fn capabilities_declare_full_support() {
    let caps = LanguageBackend::capabilities(&WasmGenerator);
    assert_eq!(caps, TargetCapabilities::full());
}

#[test]
fn listeners_emit_register_unregister_in_js() {
    let js = js_stub_for(
        &listener_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    // One long-lived trampoline per callback typedef, in the function
    // table, decoding the borrowed string argument without freeing it.
    assert!(
        js.contains(
            "const _lsnPtr_weaveffi_events_OnMessage_fn = _registerTrampoline(_table, ['i32', 'i32'],"
        ),
        "{js}"
    );
    assert!(js.contains("const _p0 = _readCStr(wasm, a0);"), "{js}");
    assert!(js.contains("_l.callback(_p0);"), "{js}");
    // Register hands the trampoline and a context id to the producer and
    // returns the numeric context id; unregister releases both sides.
    assert!(js.contains("registerMessageListener(callback) {"), "{js}");
    assert!(
        js.contains(
            "wasm.weaveffi_events_register_message_listener(_lsnPtr_weaveffi_events_OnMessage_fn, _id)"
        ),
        "{js}"
    );
    assert!(js.contains("unregisterMessageListener(id) {"), "{js}");
    assert!(
        js.contains("wasm.weaveffi_events_unregister_message_listener(_l.rid);"),
        "{js}"
    );
    assert!(!js.contains("is not supported"), "{js}");
}

#[test]
fn listeners_declared_in_dts() {
    let api = listener_api();
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("registerMessageListener(callback: (message: string) => void): number;"),
        "{dts}"
    );
    assert!(
        dts.contains("unregisterMessageListener(id: number): void;"),
        "{dts}"
    );
    assert!(dts.contains("send(text: string)"), "{dts}");
}

#[test]
fn readme_documents_listeners() {
    let readme = readme_for(&listener_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Callbacks and Listeners"), "{readme}");
    assert!(readme.contains("synchronous"), "{readme}");
    assert!(readme.contains("subscription id"), "{readme}");
    assert!(readme.contains("buffered values"), "{readme}");
    assert!(!readme.contains("## Unsupported Features"), "{readme}");
}

#[test]
fn listener_free_api_has_no_listener_section() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(!readme.contains("### Callbacks and Listeners"));
}

#[test]
fn listeners_emit_throwing_stubs_in_emscripten_mode() {
    let js = js_stub_for(
        &listener_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    assert!(js.contains("registerMessageListener() {"), "{js}");
    assert!(js.contains("unregisterMessageListener() {"), "{js}");
    assert!(
        js.contains("listener 'message_listener' is not supported in Emscripten mode"),
        "{js}"
    );
    assert!(
        !js.contains("_lsnPtr_") && !js.contains("_listeners"),
        "no listener machinery in Emscripten mode: {js}"
    );
}

#[test]
fn listeners_omitted_from_dts_in_emscripten_mode() {
    let api = listener_api();
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        true,
    );
    assert!(!dts.contains("registerMessageListener"), "{dts}");
    assert!(dts.contains("send(text: string)"), "{dts}");
}

#[test]
fn readme_documents_listener_gap_in_emscripten_mode() {
    let readme = readme_for(&listener_api(), "weaveffi", "weaveffi.yml", true);
    assert!(readme.contains("## Callbacks and Listeners"), "{readme}");
    assert!(
        readme.contains("not supported in Emscripten mode"),
        "{readme}"
    );
}

#[test]
fn readme_documents_records_as_plain_objects() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Records"));
    assert!(readme.contains("plain JavaScript objects"));
    assert!(readme.contains("value-buffer format"));
    assert!(readme.contains("nothing to free"));
    assert!(!readme.contains("opaque handles"));
}

#[test]
fn readme_documents_enums() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Enums"));
    assert!(readme.contains("`i32` values"));
    assert!(readme.contains("discriminant"));
    assert!(readme.contains("tagged by variant name"));
}

#[test]
fn readme_documents_optionals() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Optionals"));
    assert!(readme.contains("`null`"));
    assert!(readme.contains("presence flag"));
    assert!(readme.contains("nullable object pointer"));
}

#[test]
fn readme_documents_lazy_iterators() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Iterators"));
    assert!(readme.contains("lazy JS iterator"));
    assert!(readme.contains("`return()`"));
    assert!(readme.contains("destroyed"));
}

#[test]
fn readme_documents_lists_and_maps() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Lists and Maps"));
    assert!(readme.contains("serialized in a value buffer"));
    assert!(readme.contains("pointer + length"));
}

#[test]
fn readme_documents_error_struct_layout_and_payloads() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("16 bytes on wasm32"), "{readme}");
    assert!(readme.contains("payload_ptr"), "{readme}");
    assert!(
        readme.contains("decodes them from the error's value"),
        "{readme}"
    );
}

#[test]
fn js_stub_has_jsdoc() {
    let js = js_stub_for(
        &empty_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("@param {string} url"));
    assert!(js.contains("@returns {Promise<WebAssembly.Exports>}"));
    assert!(js.contains("@example"));
}

#[test]
fn js_stub_documents_complex_types() {
    let js = js_stub_for(
        &empty_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("Record: plain objects in and out (serialized automatically)."));
    assert!(js.contains("Enum: pass the integer discriminant."));
    assert!(js.contains("Optional: pass null to omit, a value to provide."));
    assert!(js.contains("List/Map: pass arrays/objects; receive arrays/objects."));
}

#[test]
fn js_stub_has_type_convention_header() {
    let js = js_stub_for(
        &empty_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("Objects   -> i32 pointer into linear memory (0 = null/absent)"));
    assert!(js.contains("Enums     -> i32 discriminant value"));
    assert!(js.contains("Bytes     -> i32 data pointer + i32 length (out_len for returns)"));
    assert!(js.contains("Buffered  -> records, rich enums, optionals, lists, and maps cross"));
    assert!(js.contains("as one value buffer: i32 pointer + i32 length"));
}

#[test]
fn generate_writes_both_files() {
    let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen");
    let _ = std::fs::remove_dir_all(&tmp);
    let out = Utf8Path::from_path(tmp.as_path()).unwrap();
    let api = make_api(vec![]);
    WasmGenerator
        .generate(&api, out, &WasmConfig::default())
        .unwrap();

    let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
    assert!(readme.contains("## Complex Type Handling"));

    let js = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.js")).unwrap();
    assert!(js.contains("export async function loadWeaveffiWasm"));
    assert!(js.contains("@param {string} url"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn empty_api_has_no_api_reference() {
    let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
    assert!(!readme.contains("## API Reference"));
}

#[test]
fn api_reference_lists_module() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("## API Reference"));
    assert!(readme.contains("### Module: `math`"));
}

#[test]
fn api_reference_function_abi_name() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("##### `weaveffi_math_add`"));
}

#[test]
fn api_reference_function_signature() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
}

#[test]
fn api_reference_function_param_table() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("| `a` | `i32` | `i32` | native Wasm i32 |"));
    assert!(readme.contains("| `b` | `i32` | `i32` | native Wasm i32 |"));
    assert!(readme.contains("| _returns_ | `i32` | `i32` | native Wasm i32 |"));
}

#[test]
fn api_reference_function_doc() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("Add two numbers"));
}

#[test]
fn api_reference_struct_fields() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("##### `Point`"));
    assert!(readme.contains("A plain JS object, serialized in a value buffer"));
    assert!(readme.contains("| `x` | `f64` |"));
    assert!(readme.contains("| `y` | `f64` |"));
    // Records declare no C symbols: no getters, no create, no destroy.
    assert!(!readme.contains("weaveffi_math_Point"), "{readme}");
}

#[test]
fn api_reference_enum_discriminants() {
    let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("##### `Color`"));
    assert!(readme.contains("`i32` discriminant"));
    assert!(readme.contains("| `Red` | `0` |"));
    assert!(readme.contains("| `Green` | `1` |"));
    assert!(readme.contains("| `Blue` | `2` |"));
}

#[test]
fn wasm_type_maps_all_variants() {
    assert_eq!(wasm_type(&TypeRef::I32), "i32");
    assert_eq!(wasm_type(&TypeRef::U32), "i32");
    assert_eq!(wasm_type(&TypeRef::I64), "i64");
    assert_eq!(wasm_type(&TypeRef::F64), "f64");
    assert_eq!(wasm_type(&TypeRef::Bool), "i32");
    // A string is a single NUL-terminated C string pointer.
    assert_eq!(wasm_type(&TypeRef::StringUtf8), "i32");
    assert_eq!(wasm_type(&TypeRef::Bytes), "i32, i32");
    assert_eq!(wasm_type(&TypeRef::Handle), "i64");
    // Buffered value types cross as one ptr + len pair.
    assert_eq!(wasm_type(&TypeRef::Record("Foo".into())), "i32, i32");
    assert_eq!(wasm_type(&TypeRef::RichEnum("Shape".into())), "i32, i32");
    assert_eq!(wasm_type(&TypeRef::Enum("Bar".into())), "i32");
    assert_eq!(
        wasm_type(&TypeRef::List(Box::new(TypeRef::I32))),
        "i32, i32"
    );
    assert_eq!(
        wasm_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "i32, i32"
    );
    assert_eq!(
        wasm_type(&TypeRef::Optional(Box::new(TypeRef::Record("Foo".into())))),
        "i32, i32"
    );
    assert_eq!(
        wasm_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "i32, i32"
    );
    // The one non-buffered optional: a nullable interface pointer.
    assert_eq!(
        wasm_type(&TypeRef::Optional(Box::new(TypeRef::Interface("S".into())))),
        "i32"
    );
    assert_eq!(wasm_type(&TypeRef::TypedHandle("Contact".into())), "i32");
    assert_eq!(wasm_type(&TypeRef::Interface("Store".into())), "i32");
}

#[test]
fn wasm_type_note_covers_all_variants() {
    assert_eq!(wasm_type_note(&TypeRef::I32), "native Wasm i32");
    assert_eq!(wasm_type_note(&TypeRef::U32), "unsigned mapped to i32");
    assert_eq!(wasm_type_note(&TypeRef::Bool), "0 = false, 1 = true");
    assert_eq!(
        wasm_type_note(&TypeRef::StringUtf8),
        "NUL-terminated C string pointer"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::Record("X".into())),
        "value buffer: ptr + len in linear memory"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::RichEnum("X".into())),
        "value buffer: ptr + len in linear memory"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::Enum("E".into())),
        "variant discriminant"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Record("S".into())))),
        "value buffer: ptr + len in linear memory"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "value buffer: ptr + len in linear memory"
    );
    assert_eq!(
        wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Interface("S".into())))),
        "nullable object pointer, 0 = absent"
    );
}

#[test]
fn type_display_round_trips() {
    assert_eq!(type_display(&TypeRef::I32), "i32");
    assert_eq!(type_display(&TypeRef::StringUtf8), "string");
    assert_eq!(type_display(&TypeRef::Record("Foo".into())), "Foo");
    assert_eq!(type_display(&TypeRef::RichEnum("Shape".into())), "Shape");
    assert_eq!(
        type_display(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "i32?"
    );
    assert_eq!(
        type_display(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
        "[string]"
    );
    assert_eq!(
        type_display(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "{string:i32}"
    );
}

/// A `contacts` module with a string-to-optional-record lookup, reused by
/// the API-reference and marshalling tests.
fn contacts_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member(
            "find",
            vec![str_param("name")],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                field("id", TypeRef::I32),
                field("name", TypeRef::StringUtf8),
            ],
        }],
        ..module("contacts")
    }])
}

#[test]
fn api_reference_complex_types() {
    let readme = readme_for(&contacts_api(), "weaveffi", "weaveffi.yml", false);
    assert!(
        readme.contains("| `name` | `string` | `i32` | NUL-terminated C string pointer |"),
        "{readme}"
    );
    assert!(
        readme.contains(
            "| _returns_ | `Contact?` | `i32, i32` | value buffer: ptr + len in linear memory |"
        ),
        "{readme}"
    );
    assert!(
        !readme.contains("weaveffi_contacts_Contact_get"),
        "{readme}"
    );
}

#[test]
fn api_reference_void_return() {
    let api = make_api(vec![Module {
        functions: vec![member("print", vec![str_param("msg")], None, false, false)],
        ..module("io")
    }]);
    let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("-> void`"));
    assert!(!readme.contains("_returns_"));
}

#[test]
fn api_reference_multiple_modules() {
    let api = make_api(vec![module("math"), module("io")]);
    let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("### Module: `math`"));
    assert!(readme.contains("### Module: `io`"));
}

#[test]
fn generate_writes_api_reference() {
    let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen_api");
    let _ = std::fs::remove_dir_all(&tmp);
    let out = Utf8Path::from_path(tmp.as_path()).unwrap();
    let api = sample_api();
    WasmGenerator
        .generate(&api, out, &WasmConfig::default())
        .unwrap();

    let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
    assert!(readme.contains("## API Reference"));
    assert!(readme.contains("weaveffi_math_add"));
    assert!(readme.contains("##### `Point`"));
    assert!(readme.contains("##### `Color`"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn wasm_js_has_api_functions() {
    let api = sample_api();
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("add(a, b)"));
    assert!(js.contains("wasm.weaveffi_math_add(a, b, _err)"));
    // The record is a plain value shape: codecs at module scope, no
    // wrapper class, no getters, no module-object factory.
    assert!(js.contains("function _write_math_Point(w, v) {"), "{js}");
    assert!(js.contains("function _read_math_Point(r) {"), "{js}");
    assert!(js.contains("w.f64(v.x);"), "{js}");
    assert!(js.contains("v.x = r.f64();"), "{js}");
    assert!(!js.contains("class Point"), "{js}");
    assert!(!js.contains("get x()"), "{js}");
    assert!(!js.contains("Point: {"), "{js}");
    assert!(js.contains("export const Color = Object.freeze("));
    assert!(js.contains("Red: 0"));
    assert!(js.contains("Green: 1"));
    assert!(js.contains("Blue: 2"));
    assert!(js.contains("_raw: wasm"));
    assert!(js.contains("math: {"));
}

#[test]
fn wasm_js_emits_buffer_runtime_when_records_present() {
    let js = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("class _BufWriter {"), "{js}");
    assert!(js.contains("class _BufReader {"), "{js}");
    // The reader is strict: little-endian DataView reads plus validation.
    assert!(js.contains("truncated"), "{js}");
    assert!(js.contains("bool byte out of range"), "{js}");
    assert!(js.contains("option flag byte out of range"), "{js}");
    assert!(
        js.contains("length prefix exceeds remaining buffer"),
        "{js}"
    );
    assert!(js.contains("trailing bytes after value"), "{js}");
    assert!(js.contains("string is not valid UTF-8"), "{js}");
    assert!(js.contains("{ fatal: true }"), "{js}");
}

#[test]
fn wasm_generates_dts() {
    let tmp = std::env::temp_dir().join("weaveffi_test_wasm_dts");
    let _ = std::fs::remove_dir_all(&tmp);
    let out = Utf8Path::from_path(tmp.as_path()).unwrap();
    let api = sample_api();
    WasmGenerator
        .generate(&api, out, &WasmConfig::default())
        .unwrap();

    let dts = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.d.ts")).unwrap();
    assert!(dts.contains("export interface WeaveffiWasmModule"));
    assert!(
        dts.contains("export function loadWeaveffiWasm(url: string): Promise<WeaveffiWasmModule>")
    );
    assert!(dts.contains("add(a: number, b: number): number"));
    // Records are plain object interfaces: mutable fields, no free().
    assert!(dts.contains("export interface Point"));
    assert!(dts.contains("  x: number;"));
    assert!(dts.contains("  y: number;"));
    assert!(!dts.contains("readonly x"));
    assert!(!dts.contains("free(): void;"));
    assert!(dts.contains("export declare const Color"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn wasm_js_has_string_helpers() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "greet",
            vec![str_param("name")],
            Some(TypeRef::StringUtf8),
            false,
            false,
        )],
        ..module("greeting")
    }]);
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("function _cstr(wasm, str)"));
    assert!(js.contains("function _readCStr(wasm, ptr)"));
    assert!(js.contains("function _takeCStr(wasm, ptr)"));
    assert!(js.contains("TextEncoder"));
    assert!(js.contains("TextDecoder"));
    assert!(js.contains("_cstr(wasm, name)"));
    assert!(js.contains("_takeCStr(wasm,"));
    assert!(js.contains("greet(name)"));
    assert!(js.contains("wasm.weaveffi_greeting_greet("));
}

#[test]
fn wasm_js_has_error_helpers() {
    let api = sample_api();
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("function _allocErr(wasm)"));
    assert!(js.contains("function _checkErr(wasm, errPtr)"));
    // The slot is the 16-byte error struct with payload fields.
    assert!(js.contains("wasm.weaveffi_alloc(16)"));
    assert!(js.contains("wasm.weaveffi_dealloc(errPtr, 16);"));
}

#[test]
fn wasm_js_function_passes_err() {
    let api = sample_api();
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(js.contains("const _err = _allocErr(wasm)"));
    assert!(js.contains("_checkErr(wasm, _err)"));
}

#[test]
fn wasm_dts_has_throws_doc() {
    let api = sample_api();
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("@throws"),
        "Expected .d.ts to contain @throws JSDoc comment"
    );
    assert!(dts.contains("@throws {WeaveFFIError} if the native call fails"));
}

#[test]
fn wasm_custom_module_name() {
    let tmp = std::env::temp_dir().join("weaveffi_test_wasm_custom_name");
    let _ = std::fs::remove_dir_all(&tmp);
    let out = Utf8Path::from_path(tmp.as_path()).unwrap();
    let api = sample_api();
    let config = WasmConfig {
        module_name: Some("my_bindings".into()),
        ..WasmConfig::default()
    };
    WasmGenerator.generate(&api, out, &config).unwrap();

    assert!(out.join("wasm/my_bindings.js").exists());
    assert!(out.join("wasm/my_bindings.d.ts").exists());

    let js = std::fs::read_to_string(out.join("wasm/my_bindings.js")).unwrap();
    assert!(js.contains("loadMyBindings"));

    let dts = std::fs::read_to_string(out.join("wasm/my_bindings.d.ts")).unwrap();
    assert!(dts.contains("MyBindingsModule"));
    assert!(dts.contains("loadMyBindings"));

    let files = WasmGenerator.output_files(&api, out, &config);
    assert!(files.iter().any(|f| f.contains("my_bindings.js")));
    assert!(files.iter().any(|f| f.contains("my_bindings.d.ts")));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn wasm_typed_handle_type() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "get_info",
            vec![param("contact", TypeRef::TypedHandle("Contact".into()))],
            None,
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        ..module("contacts")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("contact: number"),
        "TypedHandle is an opaque i32 pointer surfaced as number: {dts}"
    );
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(
        js.contains("wasm.weaveffi_contacts_get_info(contact, _err)"),
        "TypedHandle passes through unwrapped: {js}"
    );
    assert!(!js.contains("contact._handle"), "{js}");
}

#[test]
fn wasm_deeply_nested_optional() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "process",
            vec![param(
                "data",
                TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                    Box::new(TypeRef::Record("Contact".into())),
                ))))),
            )],
            None,
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        ..module("edge")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("(Contact | null)[] | null"),
        "should contain deeply nested optional type: {dts}"
    );
}

#[test]
fn wasm_map_of_lists() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "process",
            vec![param(
                "scores",
                TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                ),
            )],
            None,
            false,
            false,
        )],
        ..module("edge")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("Record<string, number[]>"),
        "should contain map of lists type: {dts}"
    );
}

#[test]
fn wasm_enum_keyed_map() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "process",
            vec![param(
                "contacts",
                TypeRef::Map(
                    Box::new(TypeRef::Enum("Color".into())),
                    Box::new(TypeRef::Record("Contact".into())),
                ),
            )],
            None,
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        ..module("edge")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("Record<Color, Contact>"),
        "should contain enum-keyed map type: {dts}"
    );
}

/// A one-function API returning a record, exercising both the buffered
/// parameter staging and the buffered return decode.
fn record_roundtrip_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member(
            "save",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            Some(TypeRef::Record("Contact".into())),
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        ..module("contacts")
    }])
}

#[test]
fn buffered_param_staged_like_bytes_and_deallocated() {
    let js = js_for_api(&record_roundtrip_api());
    // Encode into a _BufWriter, stage via _bytes (weaveffi_alloc + copy),
    // pass (ptr, len), dealloc after the call.
    assert!(js.contains("const a0_w = new _BufWriter();"), "{js}");
    assert!(
        js.contains("_write_contacts_Contact(a0_w, contact);"),
        "{js}"
    );
    assert!(
        js.contains("const [a0_p, a0_l] = _bytes(wasm, a0_w.finish());"),
        "{js}"
    );
    assert!(
        js.contains("wasm.weaveffi_contacts_save(a0_p, a0_l, _lp, _err);"),
        "{js}"
    );
    let call = js.find("wasm.weaveffi_contacts_save(").unwrap();
    let dealloc = js.find("wasm.weaveffi_dealloc(a0_p, a0_l);").unwrap();
    assert!(call < dealloc, "staged encoding freed after the call: {js}");
}

#[test]
fn buffered_return_read_decoded_and_freed() {
    let js = js_for_api(&record_roundtrip_api());
    // The trailing out_len slot is allocated before the call and read
    // (then released) afterwards.
    assert!(js.contains("const _lp = wasm.weaveffi_alloc(4);"), "{js}");
    assert!(
        js.contains("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);"),
        "{js}"
    );
    assert!(js.contains("wasm.weaveffi_dealloc(_lp, 4);"), "{js}");
    // The buffer is copied out and freed by _takeBytes, then decoded
    // strictly (end() rejects trailing bytes).
    assert!(
        js.contains("const _rd = new _BufReader(_takeBytes(wasm, _r, _len));"),
        "{js}"
    );
    assert!(
        js.contains("const _out = _read_contacts_Contact(_rd);"),
        "{js}"
    );
    assert!(js.contains("_rd.end();"), "{js}");
    assert!(js.contains("function _takeBytes(wasm, ptr, len)"), "{js}");
    assert!(js.contains("wasm.weaveffi_free_bytes(ptr, len);"), "{js}");
    // Errors are checked before the result is decoded, and no wrapper
    // class exists for the record.
    let check = js.find("_checkErr(wasm, _err)").expect("error check");
    let decode = js
        .find("const _out = _read_contacts_Contact(_rd);")
        .expect("record decode");
    assert!(check < decode, "errors checked before decoding: {js}");
    assert!(!js.contains("class Contact"), "{js}");
}

#[test]
fn optional_record_return_uses_presence_flag() {
    let js = js_for_api(&contacts_api());
    // An optional record is buffered: a one-byte flag, then the value.
    assert!(
        js.contains("const _out = (_rd.flag() ? _read_contacts_Contact(_rd) : null);"),
        "{js}"
    );
}

#[test]
fn optional_scalar_return_decodes_from_buffer() {
    let js = js_for_api(&returning_api(
        TypeRef::Optional(Box::new(TypeRef::I32)),
        false,
    ));
    assert!(
        js.contains("const _out = (_rd.flag() ? _rd.i32() : null);"),
        "{js}"
    );
    // The old boxed-scalar protocol is gone.
    assert!(!js.contains("wasm.weaveffi_free_bytes(_r, 4);"), "{js}");
}

#[test]
fn list_return_decodes_from_buffer() {
    let js = js_for_api(&returning_api(
        TypeRef::List(Box::new(TypeRef::StringUtf8)),
        false,
    ));
    assert!(
        js.contains(
            "const _out = (() => { const _n = _rd.len(); const _arr = []; for (let _i = 0; _i < _n; _i++) _arr.push(_rd.str()); return _arr; })();"
        ),
        "{js}"
    );
    // No parallel-array protocol remains.
    assert!(!js.contains("_takeStrArray"), "{js}");
}

#[test]
fn map_return_decodes_from_buffer() {
    let js = js_for_api(&returning_api(
        TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
        false,
    ));
    assert!(
        js.contains(
            "const _out = (() => { const _n = _rd.len(); const _obj = {}; for (let _i = 0; _i < _n; _i++) { const _k = _rd.str(); _obj[_k] = _rd.i32(); } return _obj; })();"
        ),
        "{js}"
    );
    assert!(!js.contains("_ka"), "no parallel key array remains: {js}");
}

#[test]
fn list_param_serializes_elements_in_order() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "sum",
            vec![param("xs", TypeRef::List(Box::new(TypeRef::I32)))],
            Some(TypeRef::I64),
            false,
            false,
        )],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    assert!(js.contains("const _a1 = xs || [];"), "{js}");
    assert!(js.contains("a0_w.len(_a1.length);"), "{js}");
    assert!(js.contains("for (const _e1 of _a1) {"), "{js}");
    assert!(js.contains("a0_w.i32(_e1);"), "{js}");
}

#[test]
fn map_param_accepts_map_instances_and_plain_objects() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "load",
            vec![param(
                "scores",
                TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            )],
            None,
            false,
            false,
        )],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    assert!(
        js.contains("const _m1 = _s1 instanceof Map ? [..._s1.entries()] : Object.entries(_s1);"),
        "{js}"
    );
    assert!(js.contains("a0_w.len(_m1.length);"), "{js}");
    assert!(js.contains("for (const [_k1, _v1] of _m1) {"), "{js}");
    assert!(js.contains("a0_w.str(_k1);"), "{js}");
    assert!(js.contains("a0_w.i32(_v1);"), "{js}");
}

#[test]
fn optional_param_writes_presence_flag() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "set_timeout",
            vec![param("ms", TypeRef::Optional(Box::new(TypeRef::I32)))],
            None,
            false,
            false,
        )],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    assert!(
        js.contains("if (ms === null || ms === undefined) {"),
        "{js}"
    );
    assert!(js.contains("a0_w.flag(false);"), "{js}");
    assert!(js.contains("a0_w.flag(true);"), "{js}");
    assert!(js.contains("a0_w.i32(ms);"), "{js}");
}

#[test]
fn wasm_async_returns_promise() {
    let api = make_api(vec![Module {
        functions: vec![Function {
            name: "compute".into(),
            params: vec![param("x", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        ..module("math")
    }]);
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(
        js.contains("new Promise"),
        "async function should return a Promise: {js}"
    );
    assert!(
        js.contains("resolve"),
        "Promise should have resolve callback: {js}"
    );
    assert!(
        js.contains("reject"),
        "Promise should have reject callback: {js}"
    );
    assert!(
        js.contains("_asyncContexts"),
        "should use async context map: {js}"
    );
    assert!(
        js.contains("_registerTrampoline"),
        "should register trampoline in function table: {js}"
    );
    assert!(
        js.contains("weaveffi_math_compute_async("),
        "should call the _async export: {js}"
    );
    assert!(
        js.contains("__indirect_function_table"),
        "should reference the Wasm function table: {js}"
    );
}

/// The Wasm bindings register one trampoline per async-callback
/// signature on the indirect function table for the lifetime of the API
/// instance and route per-call resolve/reject through the
/// `_asyncContexts` map. Each entry is `set(ctxId, ...)` once and
/// `delete(ctxId)` once on the callback path so the resolver closures do
/// not leak.
#[test]
fn wasm_async_pins_callback_for_lifetime() {
    let api = make_api(vec![Module {
        functions: vec![Function {
            name: "compute".into(),
            params: vec![param("x", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        ..module("math")
    }]);
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    let trampoline_count = js.matches("_registerTrampoline").count();
    let set_count = js.matches("_asyncContexts.set(ctxId").count();
    let delete_count = js.matches("_asyncContexts.delete(ctxId)").count();
    // Trampoline is defined once and registered once per signature.
    assert_eq!(
        trampoline_count, 2,
        "expected one definition and one registration of the trampoline, got {trampoline_count}: {js}"
    );
    assert_eq!(
        set_count, delete_count,
        "every _asyncContexts.set must be matched by a delete: set={set_count} delete={delete_count}: {js}"
    );
    assert!(
        set_count >= 1,
        "expected at least one _asyncContexts.set per async fn: {js}"
    );
}

#[test]
fn wasm_dts_async_function() {
    let api = make_api(vec![Module {
        functions: vec![
            Function {
                name: "compute".into(),
                params: vec![param("x", TypeRef::I32)],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            member(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
                false,
                false,
            ),
        ],
        ..module("math")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("compute(x: number): Promise<number>"),
        "async function should return Promise<T> in .d.ts: {dts}"
    );
    assert!(
        dts.contains("add(a: number, b: number): number"),
        "sync function should not return Promise: {dts}"
    );
    assert!(
        !dts.contains("add(a: number, b: number): Promise"),
        "sync function must not return Promise: {dts}"
    );
}

#[test]
fn wasm_nested_module_output() {
    let api = make_api(vec![Module {
        functions: vec![member("outer_fn", vec![], Some(TypeRef::I32), false, false)],
        modules: vec![Module {
            functions: vec![member("inner_fn", vec![], Some(TypeRef::I32), false, false)],
            ..module("child")
        }],
        ..module("parent")
    }]);
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("parent:"),
        "parent module in DTS interface missing: {dts}"
    );
    assert!(
        dts.contains("child:"),
        "nested child module in DTS interface missing: {dts}"
    );
    assert!(
        dts.contains("outerFn(): number"),
        "parent function in DTS missing: {dts}"
    );
    assert!(
        dts.contains("innerFn(): number"),
        "nested child function in DTS missing: {dts}"
    );
    let js = js_stub_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    assert!(
        js.contains("weaveffi_parent_outer_fn"),
        "parent C ABI call in JS missing: {js}"
    );
    assert!(
        js.contains("weaveffi_parent_child_inner_fn"),
        "nested child C ABI call in JS missing: {js}"
    );
}

fn doc_module() -> Module {
    Module {
        functions: vec![Function {
            name: "do_thing".into(),
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: Some("the input value".into()),
            }],
            returns: Some(TypeRef::I32),
            doc: Some("Performs a thing.".into()),
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
            }],
        }],
        enums: vec![EnumDef {
            name: "Kind".into(),
            doc: Some("Kind of item.".into()),
            variants: vec![EnumVariant {
                name: "Small".into(),
                value: 0,
                doc: Some("A small one".into()),
                fields: vec![],
            }],
        }],
        ..module("docs")
    }
}

#[test]
fn wasm_emits_doc_on_function() {
    let dts = dts_for(
        &make_api(vec![doc_module()]),
        "weaveffi",
        "weaveffi.yml",
        "weaveffi.d.ts",
        false,
    );
    assert!(dts.contains("Performs a thing."), "{dts}");
}

#[test]
fn wasm_emits_doc_on_struct() {
    let dts = dts_for(
        &make_api(vec![doc_module()]),
        "weaveffi",
        "weaveffi.yml",
        "weaveffi.d.ts",
        false,
    );
    assert!(dts.contains("/** An item we track. */"), "{dts}");
}

#[test]
fn wasm_emits_doc_on_enum_variant() {
    let dts = dts_for(
        &make_api(vec![doc_module()]),
        "weaveffi",
        "weaveffi.yml",
        "weaveffi.d.ts",
        false,
    );
    assert!(dts.contains("/** Kind of item. */"), "{dts}");
    assert!(dts.contains("/** A small one */"), "{dts}");
}

#[test]
fn wasm_emits_doc_on_field() {
    let dts = dts_for(
        &make_api(vec![doc_module()]),
        "weaveffi",
        "weaveffi.yml",
        "weaveffi.d.ts",
        false,
    );
    assert!(dts.contains("/** Stable id */"), "{dts}");
}

#[test]
fn wasm_emits_doc_on_param() {
    let dts = dts_for(
        &make_api(vec![doc_module()]),
        "weaveffi",
        "weaveffi.yml",
        "weaveffi.d.ts",
        false,
    );
    assert!(dts.contains("@param x the input value"), "{dts}");
}

#[test]
fn wasm_custom_prefix_threads_to_user_symbols() {
    let js = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "myffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    // User-exported symbols honor the configured C ABI prefix.
    assert!(
        js.contains("myffi_math_add"),
        "user export should use the custom prefix: {js}"
    );
    assert!(
        !js.contains("weaveffi_math_add"),
        "user export must not hard-code the weaveffi_ prefix: {js}"
    );
    // Runtime ABI helpers exported by weaveffi-abi stay literal.
    assert!(
        js.contains("weaveffi_alloc"),
        "runtime alloc helper must stay literal: {js}"
    );
    assert!(
        js.contains("weaveffi_error_clear"),
        "runtime error_clear helper must stay literal: {js}"
    );
}

/// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
/// f64 payload, two f32 payloads, and a string + u8 payload, plus a plain
/// sibling enum and free functions taking/returning the rich enum (already
/// resolved to `TypeRef::RichEnum`) so the value-buffer marshalling is
/// exercised too.
fn rich_enum_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![
            member(
                "describe",
                vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::StringUtf8),
                false,
                false,
            ),
            member(
                "scale",
                vec![
                    param("shape", TypeRef::RichEnum("Shape".into())),
                    param("factor", TypeRef::F64),
                ],
                Some(TypeRef::RichEnum("Shape".into())),
                false,
                false,
            ),
            member(
                "sum_bytes",
                vec![param("values", TypeRef::List(Box::new(TypeRef::U8)))],
                Some(TypeRef::U64),
                false,
                false,
            ),
        ],
        enums: vec![
            EnumDef {
                name: "Shape".into(),
                doc: Some("An algebraic shape".into()),
                variants: vec![
                    EnumVariant {
                        name: "Empty".into(),
                        value: 0,
                        doc: Some("The empty shape".into()),
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Circle".into(),
                        value: 1,
                        doc: None,
                        fields: vec![field("radius", TypeRef::F64)],
                    },
                    EnumVariant {
                        name: "Rectangle".into(),
                        value: 2,
                        doc: None,
                        fields: vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                    },
                    EnumVariant {
                        name: "Labeled".into(),
                        value: 3,
                        doc: None,
                        fields: vec![
                            field("label", TypeRef::StringUtf8),
                            field("count", TypeRef::U8),
                        ],
                    },
                ],
            },
            EnumDef {
                name: "Channel".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            },
        ],
        ..module("shapes")
    }])
}

#[test]
fn wasm_rich_enum_emits_buffer_codecs() {
    let js = js_stub_for(
        &rich_enum_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    // The writer switches on the string tag and packs the i32 discriminant
    // plus the active variant's fields in declaration order.
    assert!(js.contains("function _write_shapes_Shape(w, v) {"), "{js}");
    assert!(js.contains("switch (v.tag) {"), "{js}");
    assert!(js.contains("case \"Empty\": {"), "{js}");
    assert!(js.contains("case \"Circle\": {"), "{js}");
    assert!(js.contains("w.i32(1);"), "{js}");
    assert!(js.contains("w.f64(v.radius);"), "{js}");
    assert!(js.contains("w.f32(v.width);"), "{js}");
    assert!(js.contains("w.str(v.label);"), "{js}");
    assert!(js.contains("w.u8(v.count);"), "{js}");
    assert!(js.contains("unknown Shape variant tag"), "{js}");
    // The reader switches on the numeric tag and rebuilds the tagged
    // plain object.
    assert!(js.contains("function _read_shapes_Shape(r) {"), "{js}");
    assert!(js.contains("const _tag = r.i32();"), "{js}");
    assert!(js.contains("return { tag: \"Empty\" };"), "{js}");
    assert!(js.contains("const v = { tag: \"Circle\" };"), "{js}");
    assert!(js.contains("v.radius = r.f64();"), "{js}");
    assert!(js.contains("unknown Shape tag"), "{js}");
    // No handle-wrapper machinery remains.
    assert!(!js.contains("class Shape"), "{js}");
    assert!(!js.contains("Shape.Tag"), "{js}");
    assert!(!js.contains("Shape_destroy"), "{js}");
    assert!(!js.contains("Shape_Circle_new"), "{js}");
}

#[test]
fn wasm_rich_enum_not_emitted_as_plain_enum_object() {
    let js = js_stub_for(
        &rich_enum_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    // The rich enum must NOT be emitted as a by-value discriminant object.
    assert!(
        !js.contains("export const Shape = Object.freeze("),
        "rich enum must not be a plain enum object: {js}"
    );
    // A plain sibling enum is still emitted the by-value way.
    assert!(
        js.contains("export const Channel = Object.freeze("),
        "plain enum should still be a frozen object: {js}"
    );
}

#[test]
fn wasm_rich_enum_function_marshals_value_buffer() {
    let js = js_stub_for(
        &rich_enum_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    // A rich enum crosses the ABI as a value buffer: encoded on the way
    // in, staged like bytes, decoded on the way out.
    assert!(js.contains("_write_shapes_Shape(a0_w, shape);"), "{js}");
    assert!(
        js.contains("wasm.weaveffi_shapes_describe(a0_p, a0_l, _err);"),
        "describe must pass the staged (ptr, len) pair: {js}"
    );
    assert!(
        js.contains("wasm.weaveffi_shapes_scale(a0_p, a0_l, factor, _lp, _err);"),
        "scale must pass the pair, the scalar, and the out_len slot: {js}"
    );
    assert!(
        js.contains("const _out = _read_shapes_Shape(_rd);"),
        "scale must decode the returned buffer: {js}"
    );
    // Errors are checked before the result is decoded.
    let check = js
        .find("_checkErr(wasm, _err)")
        .expect("scale should check the error slot");
    let decode = js
        .find("const _out = _read_shapes_Shape(_rd);")
        .expect("scale should decode the result");
    assert!(
        check < decode,
        "errors must be checked before decoding: {js}"
    );
}

#[test]
fn wasm_rich_enum_dts_union() {
    let dts = dts_for(
        &rich_enum_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    // A discriminated union of plain object shapes, keyed by `tag`.
    assert!(dts.contains("export type Shape ="), "{dts}");
    assert!(dts.contains("| { tag: \"Empty\" }"), "{dts}");
    assert!(
        dts.contains("| { tag: \"Circle\"; radius: number }"),
        "{dts}"
    );
    assert!(
        dts.contains("| { tag: \"Rectangle\"; width: number; height: number }"),
        "{dts}"
    );
    assert!(
        dts.contains("| { tag: \"Labeled\"; label: string; count: number };"),
        "{dts}"
    );
    // Not a class, not a by-value const map.
    assert!(!dts.contains("export declare class Shape"), "{dts}");
    assert!(!dts.contains("export declare const Shape"), "{dts}");
    assert!(
        dts.contains("scale(shape: Shape, factor: number): Shape"),
        "functions should reference the union type: {dts}"
    );
}

#[test]
fn wasm_rich_enum_readme() {
    let readme = readme_for(&rich_enum_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("##### `Shape`"), "{readme}");
    assert!(
        readme.contains("Rich (algebraic) enum"),
        "rich enum readme should call it out: {readme}"
    );
    assert!(
        readme.contains("| Variant | Tag | Fields |"),
        "rich enum readme should tabulate variants: {readme}"
    );
    assert!(
        readme.contains("`radius: f64`"),
        "rich enum readme should list field types: {readme}"
    );
}

/// A one-function async API for the Emscripten stub tests.
fn async_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![Function {
            name: "compute".into(),
            params: vec![param("x", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        ..module("math")
    }])
}

#[test]
fn emscripten_loader_accepts_module_and_binds_exports() {
    let js = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    assert!(
        js.contains("export async function loadWeaveffiWasm(module) {"),
        "loader should accept the Emscripten module: {js}"
    );
    assert!(
        js.contains("const m = await Promise.resolve(module);"),
        "loader should accept the MODULARIZE factory promise too: {js}"
    );
    assert!(
        !js.contains("fetch(url)") && !js.contains("WebAssembly.instantiate"),
        "Emscripten mode must not instantiate the wasm itself: {js}"
    );
    // Runtime helpers and business symbols bind from the underscore-
    // prefixed Module properties, in quoted bracket notation.
    assert!(
        js.contains("weaveffi_alloc: m['_weaveffi_alloc'],"),
        "missing alloc binding: {js}"
    );
    assert!(
        js.contains("weaveffi_math_add: m['_weaveffi_math_add'],"),
        "missing business symbol binding: {js}"
    );
    // Records declare no C symbols, so nothing Point-related is bound.
    assert!(
        !js.contains("m['_weaveffi_math_Point"),
        "records must not bind any symbols: {js}"
    );
    assert!(
        js.contains("get memory() { return { buffer: m['HEAPU8'].buffer }; },"),
        "memory must be a live getter over HEAPU8: {js}"
    );
}

#[test]
fn emscripten_body_stays_identical_to_standard_mode() {
    let standard = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    );
    let emscripten = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    // The adapter confines the divergence to the loader prologue; every
    // call site keeps the same dot access on the bound `wasm` object.
    assert!(
        emscripten.contains("wasm.weaveffi_math_add(a, b, _err)"),
        "call sites must not fork per mode: {emscripten}"
    );
    for helper in ["function _cstr(wasm, str)", "function _allocErr(wasm)"] {
        let body = |s: &str| {
            let start = s.find(helper).unwrap_or_else(|| panic!("missing {helper}"));
            s[start..s[start..].find("\n\n").map_or(s.len(), |e| start + e)].to_string()
        };
        assert_eq!(
            body(&standard),
            body(&emscripten),
            "shared helpers must be byte-identical between modes"
        );
    }
}

#[test]
fn emscripten_binds_prefixed_runtime_helpers() {
    let js = js_stub_for(
        &sample_api(),
        DEFAULT_MODULE_NAME,
        "acme",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    // The glue's hardcoded helper names bind to the producer's prefixed
    // exports, matching the runtime declarations in the generated header.
    assert!(
        js.contains("weaveffi_alloc: m['_acme_alloc'],"),
        "alloc must map to the prefixed export: {js}"
    );
    assert!(
        js.contains("weaveffi_error_clear: m['_acme_error_clear'],"),
        "error_clear must map to the prefixed export: {js}"
    );
    assert!(
        js.contains("weaveffi_free_bytes: m['_acme_free_bytes'],"),
        "free_bytes must map to the prefixed export: {js}"
    );
}

#[test]
fn emscripten_async_functions_become_throwing_stubs() {
    let js = js_stub_for(
        &async_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    assert!(
        js.contains("async function 'compute' is not supported in Emscripten mode"),
        "async stub should throw with a clear message: {js}"
    );
    assert!(
        !js.contains("_registerTrampoline") && !js.contains("WebAssembly.Function"),
        "no trampoline machinery in Emscripten mode: {js}"
    );
    assert!(
        !js.contains("weaveffi_math_compute_async"),
        "the async launcher must not be bound or called: {js}"
    );
}

#[test]
fn emscripten_dts_loader_signature_and_async_omission() {
    let dts = dts_for(
        &async_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        true,
    );
    assert!(
        dts.contains(
            "export function loadWeaveffiWasm(module: object | Promise<object>): \
             Promise<WeaveffiWasmModule>;"
        ),
        "loader signature should take the Emscripten module: {dts}"
    );
    assert!(
        !dts.contains("compute("),
        "async stubs must be omitted from the d.ts: {dts}"
    );
    assert!(
        dts.contains("_raw: Record<string, unknown>;"),
        "_raw is the export-binding object in Emscripten mode: {dts}"
    );
}

#[test]
fn emscripten_readme_documents_emcc_build() {
    let readme = readme_for(&async_api(), "weaveffi", "weaveffi.yml", true);
    assert!(
        readme.contains("emcc"),
        "readme should show an emcc invocation: {readme}"
    );
    assert!(
        readme.contains("EXPORTED_RUNTIME_METHODS=HEAPU8"),
        "readme should list the required runtime method export: {readme}"
    );
    assert!(
        readme.contains("Async functions are not supported in Emscripten mode"),
        "readme should call out the async gap: {readme}"
    );
}

#[test]
fn dts_bytes_map_to_uint8array() {
    assert_eq!(ts_type_for(&TypeRef::Bytes), "Uint8Array");
    assert_eq!(ts_type_for(&TypeRef::BorrowedBytes), "Uint8Array");
}

// --- Interfaces, typed errors, throws split, naming ---

/// A kvstore-shaped module: a `Store` interface (canonical `new` plus an
/// `open` factory, sync/iterator/async methods, one static), a `KvError`
/// domain, and one non-throwing free function.
fn kv_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member("flush_all", vec![], None, false, false)],
        errors: Some(weaveffi_ir::ir::ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                weaveffi_ir::ir::ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: None,
                    fields: vec![],
                },
                weaveffi_ir::ir::ErrorCode {
                    name: "StoreFull".into(),
                    code: 1003,
                    message: "store is full".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        interfaces: vec![weaveffi_ir::ir::InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store handle.".into()),
            constructors: vec![
                member("new", vec![str_param("path")], None, true, false),
                member("open", vec![str_param("path")], None, true, false),
            ],
            methods: vec![
                member(
                    "put",
                    vec![str_param("key"), param("ttl_seconds", TypeRef::I64)],
                    None,
                    true,
                    false,
                ),
                member(
                    "get",
                    vec![str_param("key")],
                    Some(TypeRef::StringUtf8),
                    true,
                    false,
                ),
                member(
                    "list_keys",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                    false,
                    false,
                ),
                member("compact", vec![], None, true, true),
            ],
            statics: vec![member(
                "default_capacity",
                vec![],
                Some(TypeRef::U64),
                false,
                false,
            )],
        }],
        ..module("kv")
    }])
}

fn kv_js() -> String {
    js_stub_for(
        &kv_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    )
}

#[test]
fn interface_class_has_ctor_wrap_free_and_members() {
    let js = kv_js();
    assert!(js.contains("class Store {"), "{js}");
    // Canonical `new` becomes `constructor`, assigning the owned handle.
    assert!(js.contains("constructor(path) {"), "{js}");
    assert!(
        js.contains("const _r = wasm.weaveffi_kv_Store_new(a0_p, _err);"),
        "{js}"
    );
    assert!(js.contains("this._handle = _r;"), "{js}");
    // Internal adoption path used by returns and element decoding.
    assert!(js.contains("static _wrap(handle) {"), "{js}");
    assert!(
        js.contains("const _o = Object.create(Store.prototype);"),
        "{js}"
    );
    // Non-canonical constructor is a static factory returning a wrapped
    // owned handle via the ordinary return path.
    assert!(js.contains("static open(path) {"), "{js}");
    assert!(js.contains("return Store._wrap(_r);"), "{js}");
    // Methods pass the instance handle as the implicit leading argument.
    assert!(js.contains("put(key, ttlSeconds) {"), "{js}");
    assert!(
        js.contains("wasm.weaveffi_kv_Store_put(this._handle, "),
        "{js}"
    );
    // Statics are static methods.
    assert!(js.contains("static defaultCapacity() {"), "{js}");
    // Disposal: free() releases exactly once.
    assert!(js.contains("free() {"), "{js}");
    assert!(
        js.contains("wasm.weaveffi_kv_Store_destroy(this._handle);"),
        "{js}"
    );
    // The class itself is exposed on the module object.
    assert!(js.contains("Store: Store,"), "{js}");
}

#[test]
fn interface_iterator_member_returns_lazy_iterator_with_self() {
    let js = kv_js();
    assert!(js.contains("listKeys() {"), "{js}");
    // The launch call threads the instance handle and the throws-aware
    // error slot.
    assert!(
        js.contains("const _it = wasm.weaveffi_kv_Store_list_keys(this._handle, _err);"),
        "{js}"
    );
    // The wrapper hands the handle to the lazy iterator instead of
    // draining it into an array.
    assert!(
        js.contains("return new _WeaveFFIIterator(wasm, _it, 4,"),
        "{js}"
    );
    assert!(
        js.contains(
            "(it, slot, ep) => wasm.weaveffi_kv_Store_ListKeysIterator_next(it, slot, ep),"
        ),
        "{js}"
    );
    assert!(
        js.contains("(it) => wasm.weaveffi_kv_Store_ListKeysIterator_destroy(it),"),
        "{js}"
    );
    // No eager while-drain remains anywhere in the glue.
    assert!(!js.contains("while (wasm."), "{js}");
}

#[test]
fn lazy_iterator_class_implements_protocol_and_destroys_once() {
    let js = kv_js();
    assert!(js.contains("class _WeaveFFIIterator {"), "{js}");
    // Iterator protocol: next(), return() for early exit, and
    // [Symbol.iterator]() making it iterable.
    assert!(js.contains("  next() {"), "{js}");
    assert!(js.contains("  return(value) {"), "{js}");
    assert!(js.contains("  [Symbol.iterator]() {"), "{js}");
    // One producer next call per consumer step.
    assert!(
        js.contains("_has = this._callNext(this._handle, this._slot, _err);"),
        "{js}"
    );
    // Destroy exactly once: _close() nulls the handle, and every path
    // (exhaustion, next error, early return) funnels through it.
    assert!(js.contains("if (this._handle === 0) return;"), "{js}");
    assert!(js.contains("this._destroyFn(this._handle);"), "{js}");
    assert_eq!(js.matches("this._close();").count(), 3, "{js}");
    // Abandonment leak is documented at the class site.
    assert!(js.contains("leaks the"), "{js}");
}

#[test]
fn lazy_iterator_frees_string_elements_per_plan() {
    let js = kv_js();
    // Each yielded string element is copied out of wasm memory and then
    // freed with the runtime's free_string (via _takeCStr).
    assert!(
        js.contains("(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true)));"),
        "{js}"
    );
}

#[test]
fn lazy_iterator_next_errors_follow_error_strategy() {
    let js = kv_js();
    // list_keys does not throw, so both launch and next route through the
    // generic trap checker.
    let list_keys = js
        .split("listKeys() {")
        .nth(1)
        .and_then(|s| s.split("\n  }").next())
        .expect("listKeys body");
    assert!(list_keys.contains("_checkErr(wasm, _err);"), "{list_keys}");
    assert!(
        list_keys.contains("_checkErr, (w, p) =>"),
        "next checker must match the function's error strategy: {list_keys}"
    );
}

#[test]
fn typed_error_classes_and_factory() {
    let js = kv_js();
    assert!(
        js.contains("export class WeaveFFIError extends Error {"),
        "{js}"
    );
    assert!(
        js.contains("export class KvError extends WeaveFFIError {}"),
        "{js}"
    );
    assert!(
        js.contains("export class KeyNotFound extends KvError {"),
        "{js}"
    );
    assert!(js.contains("KeyNotFound.CODE = 1001;"), "{js}");
    assert!(js.contains("KvError.KeyNotFound = KeyNotFound;"), "{js}");
    assert!(js.contains("StoreFull.CODE = 1003;"), "{js}");
    // The factory takes the payload slots (unused here: no code declares
    // fields) and maps unknown codes to the generic brand error.
    assert!(
        js.contains("function _kvErrorFrom(wasm, code, message, payloadPtr, payloadLen) {"),
        "{js}"
    );
    assert!(js.contains("const _cls = _KV_ERROR_CODES[code];"), "{js}");
    assert!(js.contains("new WeaveFFIError(code, message);"), "{js}");
}

#[test]
fn throws_split_selects_typed_or_generic_checker() {
    let js = kv_js();
    // Throwing members route the out-err slot through the domain checker,
    // which reads all four error-struct fields (code, message, payload).
    assert!(
        js.contains("function _checkKvError(wasm, errPtr) {"),
        "{js}"
    );
    assert!(js.contains("_checkKvError(wasm, _err);"), "{js}");
    assert!(
        js.contains(
            "const _e = _kvErrorFrom(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));"
        ),
        "{js}"
    );
    // The non-throwing free function keeps the generic checker.
    assert!(js.contains("flushAll() {"), "{js}");
    let flush = js
        .split("flushAll() {")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("flushAll body");
    assert!(flush.contains("_checkErr(wasm, _err);"), "{flush}");
    assert!(!flush.contains("_checkKvError"), "{flush}");
}

#[test]
fn async_throwing_member_rejects_with_domain_error() {
    let js = kv_js();
    // The async context carries the domain factory for typed rejection.
    assert!(
        js.contains("_asyncContexts.set(ctxId, { resolve, reject, mkErr: _kvErrorFrom });"),
        "{js}"
    );
    assert!(
        js.contains("if (errPtr !== 0) _checkErrRef(wasm, errPtr, ctx.mkErr);"),
        "{js}"
    );
    // The boxed-error checker hands the payload slots to the factory, then
    // releases the box before throwing.
    assert!(
        js.contains(
            "const err = mkErr ? mkErr(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true)) : new WeaveFFIError(code, msg);"
        ),
        "{js}"
    );
    assert!(
        js.contains("wasm.weaveffi_error_free(errPtr);"),
        "the consumer must free the boxed async error: {js}"
    );
    // The launcher passes the cancel slot and callback as usual.
    assert!(
        js.contains(
            "wasm.weaveffi_kv_Store_compact_async(this._handle, 0, _cbPtr_i32_i32, ctxId);"
        ),
        "{js}"
    );
}

#[test]
fn naming_lower_camel_functions_and_params() {
    let js = kv_js();
    assert!(js.contains("flushAll() {"), "{js}");
    assert!(js.contains("put(key, ttlSeconds) {"), "{js}");
    assert!(!js.contains("ttl_seconds"), "{js}");
    assert!(!js.contains("list_keys() {"), "{js}");
}

#[test]
fn kv_dts_declares_errors_interface_and_throws_tags() {
    let dts = dts_for(
        &kv_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("export declare class WeaveFFIError extends Error {"),
        "{dts}"
    );
    assert!(
        dts.contains("export declare class KvError extends WeaveFFIError {"),
        "{dts}"
    );
    assert!(
        dts.contains("static readonly KeyNotFound: typeof KeyNotFound;"),
        "{dts}"
    );
    assert!(
        dts.contains("export declare class KeyNotFound extends KvError {"),
        "{dts}"
    );
    assert!(dts.contains("static readonly CODE: 1001;"), "{dts}");
    assert!(dts.contains("export declare class Store {"), "{dts}");
    assert!(dts.contains("constructor(path: string);"), "{dts}");
    assert!(dts.contains("static open(path: string): Store;"), "{dts}");
    assert!(
        dts.contains("put(key: string, ttlSeconds: bigint): void;"),
        "{dts}"
    );
    assert!(
        dts.contains("listKeys(): IterableIterator<string>;"),
        "{dts}"
    );
    assert!(
        dts.contains("@returns A lazy iterator"),
        "iterator members should document the streaming contract: {dts}"
    );
    assert!(dts.contains("compact(): Promise<void>;"), "{dts}");
    assert!(dts.contains("static defaultCapacity(): bigint;"), "{dts}");
    assert!(dts.contains("free(): void;"), "{dts}");
    assert!(dts.contains("Store: typeof Store;"), "{dts}");
    assert!(
        dts.contains("@throws {KvError} on a domain error code"),
        "{dts}"
    );
    assert!(
        dts.contains("@throws {WeaveFFIError} if the native call fails"),
        "{dts}"
    );
}

#[test]
fn kv_readme_documents_error_domain_and_interface() {
    let readme = readme_for(&kv_api(), "weaveffi", "weaveffi.yml", false);
    assert!(readme.contains("Error Domain: `KvError`"), "{readme}");
    assert!(
        readme.contains("| `KeyNotFound` | `1001` | key not found | (none) |"),
        "{readme}"
    );
    assert!(readme.contains("##### `Store`"), "{readme}");
    assert!(readme.contains("weaveffi_kv_Store_destroy"), "{readme}");
}

#[test]
fn emscripten_binds_interface_member_and_destroy_symbols() {
    let js = js_stub_for(
        &kv_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        true,
    );
    assert!(
        js.contains("weaveffi_kv_Store_put: m['_weaveffi_kv_Store_put'],"),
        "{js}"
    );
    assert!(
        js.contains("weaveffi_kv_Store_destroy: m['_weaveffi_kv_Store_destroy'],"),
        "{js}"
    );
    // The async member is a throwing stub; its launcher is never bound.
    assert!(
        js.contains("async function 'compact' is not supported in Emscripten mode"),
        "{js}"
    );
    assert!(!js.contains("weaveffi_kv_Store_compact_async"), "{js}");
    // Iterator surface symbols are bound so the lazy wrapper can call them.
    assert!(
        js.contains(
            "weaveffi_kv_Store_ListKeysIterator_next: m['_weaveffi_kv_Store_ListKeysIterator_next'],"
        ),
        "{js}"
    );
}

#[test]
fn optional_interface_stays_nullable_pointer() {
    let api = make_api(vec![Module {
        interfaces: vec![weaveffi_ir::ir::InterfaceDef {
            name: "Session".into(),
            doc: None,
            constructors: vec![member("new", vec![], None, false, false)],
            methods: vec![member(
                "find",
                vec![param(
                    "other",
                    TypeRef::Optional(Box::new(TypeRef::Interface("Session".into()))),
                )],
                Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Session".into(),
                )))),
                false,
                false,
            )],
            statics: vec![],
        }],
        ..module("net")
    }]);
    let js = js_for_api(&api);
    // An optional interface is the one non-buffered optional: a nullable
    // borrowed pointer in, a nullable owned pointer out.
    assert!(js.contains("(other ? other._handle : 0)"), "{js}");
    assert!(
        js.contains("return _r === 0 ? null : Session._wrap(_r);"),
        "{js}"
    );
    let dts = dts_for(
        &api,
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("find(other: Session | null): Session | null;"),
        "{dts}"
    );
}

// --- Structured error payloads ---

/// An error domain where one code declares payload fields, plus a
/// throwing function so the checker path is generated.
fn payload_api() -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member("login", vec![str_param("name")], None, true, false)],
        errors: Some(weaveffi_ir::ir::ErrorDomain {
            name: "AuthError".into(),
            codes: vec![
                weaveffi_ir::ir::ErrorCode {
                    name: "LockedOut".into(),
                    code: 1001,
                    message: "locked out".into(),
                    doc: None,
                    fields: vec![
                        field("retry_after_secs", TypeRef::I32),
                        field("user", TypeRef::StringUtf8),
                    ],
                },
                weaveffi_ir::ir::ErrorCode {
                    name: "Denied".into(),
                    code: 1002,
                    message: "denied".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        ..module("auth")
    }])
}

#[test]
fn error_payload_fields_decoded_and_attached() {
    let js = js_for_api(&payload_api());
    // The factory decodes the borrowed payload buffer per code and
    // attaches the fields as properties on the thrown error.
    assert!(
        js.contains("function _authErrorFrom(wasm, code, message, payloadPtr, payloadLen) {"),
        "{js}"
    );
    assert!(js.contains("case 1001: {"), "{js}");
    assert!(js.contains("_e.retry_after_secs = _rd.i32();"), "{js}");
    assert!(js.contains("_e.user = _rd.str();"), "{js}");
    assert!(js.contains("_rd.end();"), "{js}");
    // The payload-free code has no decode arm.
    assert!(!js.contains("case 1002: {"), "{js}");
    // The checker hands the payload slots (offsets 8 and 12 of the
    // 16-byte struct) to the factory before clearing the error.
    assert!(
        js.contains("function _checkAuthError(wasm, errPtr) {"),
        "{js}"
    );
    let checker = js
        .split("function _checkAuthError(wasm, errPtr) {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("checker body");
    let decode = checker
        .find("const _e = _authErrorFrom(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));")
        .expect("checker decodes payload");
    let clear = checker
        .find("wasm.weaveffi_error_clear(errPtr);")
        .expect("clear");
    assert!(
        decode < clear,
        "payload must be decoded before error_clear frees it: {checker}"
    );
}

#[test]
fn error_payload_fields_declared_in_dts() {
    let dts = dts_for(
        &payload_api(),
        DEFAULT_MODULE_NAME,
        "weaveffi.yml",
        "weaveffi_wasm.d.ts",
        false,
    );
    assert!(
        dts.contains("export declare class LockedOut extends AuthError {"),
        "{dts}"
    );
    assert!(dts.contains("readonly retry_after_secs: number;"), "{dts}");
    assert!(dts.contains("readonly user: string;"), "{dts}");
}

#[test]
fn error_payload_fields_listed_in_readme() {
    let readme = readme_for(&payload_api(), "weaveffi", "weaveffi.yml", false);
    assert!(
        readme.contains(
            "| `LockedOut` | `1001` | locked out | `retry_after_secs: i32`, `user: string` |"
        ),
        "{readme}"
    );
    assert!(
        readme.contains("| `Denied` | `1002` | denied | (none) |"),
        "{readme}"
    );
}

// --- Buffered iterators and callbacks ---

#[test]
fn iterator_buffered_elements_decode_then_free() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "scan",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
            false,
            false,
        )],
        structs: vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![field("id", TypeRef::I32)],
        }],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    // A buffered element writes through two out slots (ptr at p, len at
    // p + 4), so the slot is 8 bytes and next threads both pointers.
    assert!(
        js.contains("return new _WeaveFFIIterator(wasm, _it, 8,"),
        "{js}"
    );
    assert!(js.contains("_next(it, slot, slot + 4, ep),"), "{js}");
    // Each element is copied out and freed (via _takeBytes ->
    // weaveffi_free_bytes), then strictly decoded.
    assert!(
        js.contains(
            "const _rd = new _BufReader(_takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true)));"
        ),
        "{js}"
    );
    assert!(js.contains("const _v = _read_m_Entry(_rd);"), "{js}");
    assert!(js.contains("_rd.end(); return _v;"), "{js}");
}

#[test]
fn callback_buffered_argument_decoded_borrowed() {
    let api = make_api(vec![Module {
        structs: vec![StructDef {
            name: "Msg".into(),
            doc: None,
            fields: vec![field("text", TypeRef::StringUtf8)],
        }],
        callbacks: vec![weaveffi_ir::ir::CallbackDef {
            name: "OnMessage".into(),
            params: vec![param("msg", TypeRef::Record("Msg".into()))],
            doc: None,
        }],
        listeners: vec![weaveffi_ir::ir::ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        ..module("events")
    }]);
    let js = js_for_api(&api);
    // The buffered argument occupies two i32 slots plus the context slot.
    assert!(
        js.contains("_registerTrampoline(_table, ['i32', 'i32', 'i32'],"),
        "{js}"
    );
    // Borrowed: the encoding is copied out of wasm memory (never freed)
    // and decoded before the subscriber runs.
    assert!(
        js.contains(
            "const _p0_b = (a0 === 0 || a1 === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, a0, a1).slice();"
        ),
        "{js}"
    );
    assert!(js.contains("const _p0 = _read_events_Msg(_p0_r);"), "{js}");
    assert!(js.contains("_p0_r.end();"), "{js}");
    assert!(js.contains("_l.callback(_p0);"), "{js}");
}

// --- Async completion contract: borrowed buffers are copied, not freed ---

/// A one-module API with a single free function of the given return type.
fn returning_api(ret: TypeRef, is_async: bool) -> ResolvedApi {
    make_api(vec![Module {
        functions: vec![member("get_it", vec![], Some(ret), false, is_async)],
        ..module("m")
    }])
}

fn js_for_api(api: &ResolvedApi) -> String {
    js_stub_for(
        api,
        DEFAULT_MODULE_NAME,
        "weaveffi",
        "weaveffi.yml",
        "weaveffi_wasm.js",
        false,
    )
}

#[test]
fn async_string_result_is_copied_then_freed() {
    let js = js_for_api(&returning_api(TypeRef::StringUtf8, true));
    assert!(js.contains("unwrap: (w, p) => {"), "{js}");
    assert!(
        js.contains("const _s = _readCStr(w, p);"),
        "async string results are copied out of wasm memory: {js}"
    );
    assert!(
        js.contains("if (p !== 0) w.weaveffi_free_string(p);"),
        "async string results are owned and must be freed: {js}"
    );
}

#[test]
fn async_bytes_result_is_copied_then_freed() {
    let js = js_for_api(&returning_api(TypeRef::Bytes, true));
    assert!(
        js.contains("new Uint8Array(w.memory.buffer, ptr, len).slice();"),
        "async bytes results must be deep-copied: {js}"
    );
    assert!(
        js.contains("if (ptr !== 0) w.weaveffi_free_bytes(ptr, len);"),
        "async bytes results are owned and must be freed: {js}"
    );
}

#[test]
fn async_buffered_result_decoded_inside_callback_then_freed() {
    let js = js_for_api(&returning_api(
        TypeRef::List(Box::new(TypeRef::StringUtf8)),
        true,
    ));
    // The owned value buffer arrives as (ptr, len): the callback copies it
    // out of wasm memory, frees the producer allocation, then decodes.
    assert!(js.contains("unwrap: (w, ptr, len) => {"), "{js}");
    assert!(
        js.contains(
            "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();"
        ),
        "{js}"
    );
    assert!(
        js.contains("if (ptr !== 0) w.weaveffi_free_bytes(ptr, len);"),
        "the consumer frees the owned result buffer: {js}"
    );
    assert!(js.contains("_arr.push(_rd.str())"), "{js}");
    assert!(js.contains("_rd.end();"), "{js}");
    // The completion callback carries (ctx, err, ptr, len): four i32s.
    assert!(js.contains("_cbPtr_i32_i32_i32_i32"), "{js}");
    assert!(
        js.contains("wasm.weaveffi_m_get_it_async(0, _cbPtr_i32_i32_i32_i32, ctxId);"),
        "{js}"
    );
}

#[test]
fn async_optional_scalar_result_decodes_from_buffer() {
    let js = js_for_api(&returning_api(
        TypeRef::Optional(Box::new(TypeRef::I32)),
        true,
    ));
    assert!(
        js.contains("const _v = (_rd.flag() ? _rd.i32() : null);"),
        "async optional scalars decode from the borrowed buffer: {js}"
    );
}

#[test]
fn async_record_result_decoded_from_buffer() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "get_it",
            vec![],
            Some(TypeRef::Record("Contact".into())),
            false,
            true,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("id", TypeRef::I32)],
        }],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    // A record result is a borrowed value buffer, decoded in the callback.
    assert!(js.contains("const _v = _read_m_Contact(_rd);"), "{js}");
    assert!(!js.contains("new Contact(w, h)"), "{js}");
}

#[test]
fn async_interface_result_is_adopted() {
    let api = make_api(vec![Module {
        functions: vec![member(
            "connect",
            vec![],
            Some(TypeRef::Interface("Session".into())),
            false,
            true,
        )],
        interfaces: vec![weaveffi_ir::ir::InterfaceDef {
            name: "Session".into(),
            doc: None,
            constructors: vec![member("new", vec![], None, false, false)],
            methods: vec![],
            statics: vec![],
        }],
        ..module("net")
    }]);
    let js = js_for_api(&api);
    // An owned-object result transfers ownership: the callback adopts the
    // pointer into a wrapper whose free() calls the destroy symbol.
    assert!(
        js.contains("unwrap: (w, h) => Session._wrap(h) });"),
        "{js}"
    );
}

// --- Regression tests for shared-framework behavior fixes ---

#[test]
fn reserved_word_params_are_escaped() {
    // `new` and `delete` are JS reserved words; before the shared
    // `lang::escape_ident` adoption the emitted wrapper declared them
    // verbatim, producing a syntax error at load time.
    let api = make_api(vec![Module {
        functions: vec![member(
            "configure",
            vec![param("new", TypeRef::Bool), param("delete", TypeRef::Bool)],
            None,
            false,
            false,
        )],
        ..module("m")
    }]);
    let js = js_for_api(&api);
    assert!(js.contains("configure(new_, delete_) {"), "{js}");
    assert!(js.contains("new_ ? 1 : 0"), "{js}");
    assert!(js.contains("delete_ ? 1 : 0"), "{js}");
    let dts = dts_for(&api, DEFAULT_MODULE_NAME, "weaveffi.yml", "w.d.ts", false);
    assert!(
        dts.contains("configure(new_: boolean, delete_: boolean): void;"),
        "{dts}"
    );
}

#[test]
fn negative_runtime_codes_fall_through_to_brand_error() {
    // Domain codes are validated positive-only; the reserved runtime codes
    // (-1 generic, -2 panic, -3 marshalling) must never match a typed class.
    let js = kv_js();
    // The frozen table maps only the declared positive codes.
    assert!(
        js.contains("const _KV_ERROR_CODES = Object.freeze({"),
        "{js}"
    );
    assert!(js.contains("1001: KeyNotFound,"), "{js}");
    assert!(js.contains("1003: StoreFull,"), "{js}");
    assert!(
        !js.contains("-1:") && !js.contains("-2:") && !js.contains("-3:"),
        "no negative code may appear in a domain table: {js}"
    );
    // Throwing path: a table miss (every negative runtime code) builds the
    // generic brand error instead of a typed subclass.
    assert!(
        js.contains(
            "const _e = _cls ? (message ? new _cls(message) : new _cls()) : new WeaveFFIError(code, message);"
        ),
        "{js}"
    );
    // Trap path: non-throwing wrappers always surface the brand error.
    assert!(js.contains("throw new WeaveFFIError(code, msg);"), "{js}");
}

#[test]
fn package_json_escapes_user_strings() {
    use crate::package::render_wasm_package_json;
    use weaveffi_core::pkg::ResolvedPackage;

    // Before the `JsonObject` adoption these strings were interpolated
    // verbatim, so an embedded quote or backslash corrupted the manifest.
    let package = ResolvedPackage {
        name: "demo".into(),
        version: "1.0.0".into(),
        description: Some("a \"quoted\" description with a back\\slash".into()),
        license: Some("MIT OR \"Custom\"".into()),
        authors: vec!["Ada \"the\" Author".into()],
        homepage: None,
        repository: None,
    };
    let json = render_wasm_package_json(&package, "w.js", "w.d.ts", "weaveffi.yml");
    assert!(
        json.contains(r#""description": "a \"quoted\" description with a back\\slash""#),
        "{json}"
    );
    assert!(json.contains(r#""license": "MIT OR \"Custom\"""#), "{json}");
    assert!(json.contains(r#""author": "Ada \"the\" Author""#), "{json}");
}
