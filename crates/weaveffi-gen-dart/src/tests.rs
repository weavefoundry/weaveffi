//! Unit tests for the Dart generator: rendered-source assertions over
//! representative APIs, plus slot-mapping and packaging checks.

use camino::Utf8Path;
use weaveffi_core::abi::lower_param;
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::codegen::Generator;
use weaveffi_core::model::{BindingModel, ParamBinding};
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

use crate::types::{dart_type, input_slots, return_ffi, return_out_slots};
use crate::{DartConfig, DartGenerator};

fn make_api(modules: Vec<Module>) -> Api {
    Api {
        version: "0.6.0".into(),
        modules,
        generators: None,
        package: None,
    }
}

fn simple_module(functions: Vec<Function>) -> Module {
    Module {
        name: "math".into(),
        functions,
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
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

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
        default: None,
    }
}

/// A [`ParamBinding`] with its ABI slots lowered the way the model does,
/// for exercising the slot-typing helpers directly.
fn pb(name: &str, ty: TypeRef) -> ParamBinding {
    let abi = lower_param(name, &ty, "m", false);
    ParamBinding {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
        abi,
    }
}

/// Build the binding model and render the module exactly as the driver
/// does in production before calling [`LanguageBackend::files`]. Shadows
/// the production renderer for the test suite.
fn render_dart_module(api: &Api, prefix: &str, input_basename: &str) -> String {
    let model = BindingModel::build(api, prefix);
    let config = DartConfig {
        prefix: Some(prefix.to_string()),
        input_basename: Some(input_basename.to_string()),
        ..DartConfig::default()
    };
    DartGenerator.render_dart_source(api, &model, &config)
}

#[test]
fn package_bundles_native_and_rewrites_loader() {
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::LinuxArm64, "/s/linux-arm64/libcalculator.so");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &DartGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &DartConfig::default(),
    )
    .expect("dart supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("dart/native/linux-arm64/libcalculator.so")));
    let module = files
        .iter()
        .find(|f| f.path.as_str().ends_with("dart/lib/weaveffi.dart"))
        .expect("module present");
    let FileContent::Text(src) = &module.content else {
        panic!("module is text");
    };
    assert!(
        src.contains("final candidates = <String>[]")
            && src.contains("native/darwin-arm64/libcalculator.dylib"),
        "packaged loader not applied: {src}"
    );
}

#[test]
fn generator_name_is_dart() {
    assert_eq!(Generator::name(&DartGenerator), "dart");
}

#[test]
fn output_files_lists_dart_file() {
    let api = make_api(vec![]);
    let out = Utf8Path::new("/tmp/out");
    let files = DartGenerator.output_files(&api, out, &DartConfig::default());
    assert_eq!(
        files,
        vec![
            format!("{out}/dart/README.md"),
            format!("{out}/dart/lib/weaveffi.dart"),
            format!("{out}/dart/pubspec.yaml"),
        ]
    );
}

#[test]
fn dart_type_mapping() {
    assert_eq!(dart_type(&TypeRef::I32), "int");
    assert_eq!(dart_type(&TypeRef::U32), "int");
    assert_eq!(dart_type(&TypeRef::I64), "int");
    assert_eq!(dart_type(&TypeRef::F64), "double");
    assert_eq!(dart_type(&TypeRef::Bool), "bool");
    assert_eq!(dart_type(&TypeRef::StringUtf8), "String");
    assert_eq!(dart_type(&TypeRef::Handle), "int");
    assert_eq!(dart_type(&TypeRef::Record("Foo".into())), "Foo");
    assert_eq!(dart_type(&TypeRef::RichEnum("Shape".into())), "Shape");
    assert_eq!(dart_type(&TypeRef::Enum("Bar".into())), "Bar");
    assert_eq!(
        dart_type(&TypeRef::TypedHandle("Session".into())),
        "Session"
    );
    assert_eq!(
        dart_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "int?"
    );
    assert_eq!(
        dart_type(&TypeRef::List(Box::new(TypeRef::I32))),
        "List<int>"
    );
    assert_eq!(
        dart_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "Map<String, int>"
    );
}

/// Buffered types occupy one `(Pointer<Uint8>, Size)` slot pair at the
/// FFI layer, no matter how deeply they nest; direct types keep their
/// dedicated slots.
#[test]
fn input_slots_mapping() {
    let pair = |n: &str, d: &str| (n.to_string(), d.to_string());
    let buffer = vec![
        pair("Pointer<Uint8>", "Pointer<Uint8>"),
        pair("Size", "int"),
    ];
    assert_eq!(input_slots(&pb("c", TypeRef::Record("C".into()))), buffer);
    assert_eq!(input_slots(&pb("s", TypeRef::RichEnum("S".into()))), buffer);
    assert_eq!(
        input_slots(&pb("l", TypeRef::List(Box::new(TypeRef::I32)))),
        buffer
    );
    assert_eq!(
        input_slots(&pb(
            "m",
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        )),
        buffer
    );
    assert_eq!(
        input_slots(&pb("o", TypeRef::Optional(Box::new(TypeRef::I32)))),
        buffer
    );
    // Bytes share the (ptr, len) fan-out but are not a value buffer.
    assert_eq!(input_slots(&pb("b", TypeRef::Bytes)), buffer);
    assert_eq!(
        input_slots(&pb("x", TypeRef::I32)),
        vec![pair("Int32", "int")]
    );
    assert_eq!(
        input_slots(&pb("f", TypeRef::Bool)),
        vec![pair("Bool", "bool")]
    );
    assert_eq!(
        input_slots(&pb("s", TypeRef::StringUtf8)),
        vec![pair("Pointer<Utf8>", "Pointer<Utf8>")]
    );
    assert_eq!(
        input_slots(&pb("store", TypeRef::Interface("Store".into()))),
        vec![pair("Pointer<Void>", "Pointer<Void>")]
    );
    // The one optional exception: a nullable interface pointer.
    assert_eq!(
        input_slots(&pb(
            "store",
            TypeRef::Optional(Box::new(TypeRef::Interface("Store".into())))
        )),
        vec![pair("Pointer<Void>", "Pointer<Void>")]
    );
    // A typed handle's direct slot is an opaque pointer.
    assert_eq!(
        input_slots(&pb("session", TypeRef::TypedHandle("Session".into()))),
        vec![pair("Pointer<Void>", "Pointer<Void>")]
    );
}

/// Buffered and bytes returns come back as `Pointer<Uint8>` plus a
/// trailing `Pointer<Size>` out-slot; everything else keeps its direct
/// return slot with no out-params.
#[test]
fn return_slots_mapping() {
    for ty in [
        TypeRef::Record("C".into()),
        TypeRef::RichEnum("S".into()),
        TypeRef::List(Box::new(TypeRef::Record("C".into()))),
        TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
        TypeRef::Optional(Box::new(TypeRef::I64)),
        TypeRef::Bytes,
    ] {
        assert_eq!(
            return_ffi(&ty),
            ("Pointer<Uint8>".to_string(), "Pointer<Uint8>".to_string()),
            "{ty:?}"
        );
        assert_eq!(
            return_out_slots(&ty),
            vec![("Pointer<Size>".to_string(), "Pointer<Size>".to_string())],
            "{ty:?}"
        );
    }
    assert_eq!(
        return_ffi(&TypeRef::StringUtf8),
        ("Pointer<Utf8>".to_string(), "Pointer<Utf8>".to_string())
    );
    assert!(return_out_slots(&TypeRef::StringUtf8).is_empty());
    assert_eq!(
        return_ffi(&TypeRef::Optional(Box::new(TypeRef::Interface(
            "Store".into()
        )))),
        ("Pointer<Void>".to_string(), "Pointer<Void>".to_string())
    );
    assert!(return_out_slots(&TypeRef::I32).is_empty());
}

#[test]
fn generate_dart_basic() {
    let api = make_api(vec![simple_module(vec![func(
        "add",
        vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
        Some(TypeRef::I32),
    )])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dart_basic");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DartGenerator
        .generate(&api, out_dir, &DartConfig::default())
        .unwrap();

    let dart = std::fs::read_to_string(tmp.join("dart/lib/weaveffi.dart")).unwrap();

    assert!(
        dart.contains("import 'dart:ffi'"),
        "missing dart:ffi import: {dart}"
    );
    assert!(
        dart.contains("import 'package:ffi/ffi.dart'"),
        "missing ffi package import: {dart}"
    );
    assert!(
        dart.contains("import 'dart:io' show Platform"),
        "missing Platform import: {dart}"
    );
    assert!(
        dart.contains("DynamicLibrary _openLibrary()"),
        "missing _openLibrary: {dart}"
    );
    assert!(
        dart.contains("libweaveffi.dylib"),
        "missing macOS lib: {dart}"
    );
    assert!(dart.contains("libweaveffi.so"), "missing Linux lib: {dart}");
    assert!(dart.contains("weaveffi.dll"), "missing Windows lib: {dart}");
    assert!(
        dart.contains("final DynamicLibrary _lib"),
        "missing _lib: {dart}"
    );
    assert!(
        dart.contains("_WeaveFFIError extends Struct"),
        "missing error struct: {dart}"
    );
    assert!(
        dart.contains("class WeaveFFIException"),
        "missing exception class: {dart}"
    );
    assert!(dart.contains("_checkError"), "missing error check: {dart}");
    assert!(
        dart.contains("weaveffi_error_clear"),
        "missing error_clear: {dart}"
    );
    assert!(
        dart.contains("typedef _NativeWeaveffiMathAdd"),
        "missing native typedef: {dart}"
    );
    assert!(
        dart.contains("typedef _DartWeaveffiMathAdd"),
        "missing dart typedef: {dart}"
    );
    assert!(
        dart.contains("lookupFunction"),
        "missing lookupFunction: {dart}"
    );
    assert!(
        dart.contains("'weaveffi_math_add'"),
        "missing C symbol: {dart}"
    );
    assert!(
        dart.contains("Int32 Function(Int32, Int32"),
        "missing native sig: {dart}"
    );
    assert!(
        dart.contains("int Function(int, int"),
        "missing dart sig: {dart}"
    );
    assert!(
        dart.contains("int add(int a, int b)"),
        "missing wrapper: {dart}"
    );
    assert!(
        dart.contains("calloc<_WeaveFFIError>()"),
        "missing calloc: {dart}"
    );
    assert!(
        dart.contains("_checkError(err)"),
        "missing error check in wrapper: {dart}"
    );
    assert!(dart.contains("return result"), "missing return: {dart}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The library always ships the private value-buffer runtime: a
/// little-endian writer/reader pair with truncation, flag-byte, and
/// trailing-bytes validation, plus the staging/copy helpers.
#[test]
fn emits_value_buffer_runtime() {
    let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("final class _BufferWriter {"),
        "missing writer: {dart}"
    );
    assert!(
        dart.contains("final class _BufferReader {"),
        "missing reader: {dart}"
    );
    assert!(
        dart.contains("Endian.little"),
        "buffers must be little-endian: {dart}"
    );
    assert!(
        dart.contains("import 'dart:typed_data';") && dart.contains("import 'dart:convert';"),
        "missing runtime imports: {dart}"
    );
    // Decoders reject truncation, hostile lengths, bad flag bytes, and
    // trailing garbage.
    assert!(
        dart.contains("if (_remaining < n) _bufferError(context);"),
        "missing truncation check: {dart}"
    );
    assert!(
        dart.contains("length prefix exceeds remaining buffer"),
        "missing length validation: {dart}"
    );
    assert!(
        dart.contains("bool byte out of range") && dart.contains("option flag byte out of range"),
        "missing flag validation: {dart}"
    );
    assert!(
        dart.contains("trailing bytes after value"),
        "missing expectEnd validation: {dart}"
    );
    assert!(
        dart.contains("Pointer<Uint8> _stageBytes(Uint8List bytes)")
            && dart.contains("Uint8List _copyNativeBytes(Pointer<Uint8> ptr, int len)"),
        "missing staging helpers: {dart}"
    );
}

/// The error struct mirrors the C `weaveffi_error`, including the
/// structured payload slots.
#[test]
fn error_struct_has_payload_slots() {
    let api = make_api(vec![simple_module(vec![func("ping", vec![], None)])]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("external Pointer<Uint8> payloadPtr;"),
        "missing payload pointer: {dart}"
    );
    assert!(
        dart.contains("@Size()\n  external int payloadLen;"),
        "missing payload length: {dart}"
    );
}

#[test]
fn generate_dart_with_structs() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: Some("A contact record".into()),
            fields: vec![
                field("id", TypeRef::I64),
                field("first_name", TypeRef::StringUtf8),
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    // A record is a plain Dart value class with final typed fields.
    assert!(dart.contains("class Contact {"), "missing class: {dart}");
    assert!(dart.contains("final int id;"), "missing id field: {dart}");
    assert!(
        dart.contains("final String firstName;"),
        "missing firstName field: {dart}"
    );
    assert!(
        dart.contains("final String? email;"),
        "missing optional email field: {dart}"
    );
    assert!(
        dart.contains("Contact({required this.id, required this.firstName, this.email});"),
        "missing value constructor: {dart}"
    );
    // No C symbols exist for a record: no handle, no dispose, no getters,
    // no builders.
    assert!(
        !dart.contains("Contact._("),
        "record must not wrap a native handle: {dart}"
    );
    assert!(
        !dart.contains("weaveffi_contacts_Contact_destroy")
            && !dart.contains("weaveffi_contacts_Contact_get_"),
        "record must not bind C symbols: {dart}"
    );
    assert!(
        !dart.contains("ContactBuilder"),
        "builders are gone: {dart}"
    );
    // One pack and one unpack helper, fields in declaration (wire) order.
    assert!(
        dart.contains("void _packContact(_BufferWriter w, Contact v) {"),
        "missing pack helper: {dart}"
    );
    assert!(
        dart.contains("w.writeInt64(v.id);"),
        "missing i64 field write: {dart}"
    );
    assert!(
        dart.contains("w.writeString(v.firstName);"),
        "missing string field write: {dart}"
    );
    assert!(
        dart.contains("w.writeOptionFlag(false);") && dart.contains("w.writeOptionFlag(true);"),
        "missing optional flag writes: {dart}"
    );
    assert!(
        dart.contains("Contact _unpackContact(_BufferReader r) {"),
        "missing unpack helper: {dart}"
    );
    assert!(
        dart.contains("id: r.readInt64(),")
            && dart.contains("firstName: r.readString(),")
            && dart.contains("email: (r.readOptionFlag() ? r.readString() : null),"),
        "missing field reads in wire order: {dart}"
    );
}

/// A record field of optional type is not `required`; every other field
/// is.
#[test]
fn record_constructor_requires_non_optional_fields() {
    let api = make_api(vec![Module {
        name: "geo".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Point".into(),
            doc: None,
            fields: vec![
                field("x", TypeRef::F64),
                field("label", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Point({required this.x, this.label});"),
        "optional fields must not be required: {dart}"
    );
}

#[test]
fn generate_dart_with_enums() {
    let api = make_api(vec![Module {
        name: "paint".into(),
        functions: vec![func(
            "mix",
            vec![param("color", TypeRef::Enum("Color".into()))],
            Some(TypeRef::Enum("Color".into())),
        )],
        structs: vec![],
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
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    assert!(dart.contains("enum Color {"), "missing enum: {dart}");
    assert!(dart.contains("red(0)"), "missing red: {dart}");
    assert!(dart.contains("green(1)"), "missing green: {dart}");
    assert!(dart.contains("blue(2)"), "missing blue: {dart}");
    assert!(
        dart.contains("const Color(this.value)"),
        "missing const constructor: {dart}"
    );
    assert!(
        dart.contains("final int value"),
        "missing value field: {dart}"
    );
    assert!(
        dart.contains("static Color fromValue(int value)"),
        "missing fromValue: {dart}"
    );
    assert!(
        dart.contains("Color mix(Color color)"),
        "missing mix signature: {dart}"
    );
    assert!(
        dart.contains("color.value"),
        "missing .value conversion: {dart}"
    );
    assert!(
        dart.contains("Color.fromValue(result)"),
        "missing fromValue conversion: {dart}"
    );
}

#[test]
fn void_function() {
    let api = make_api(vec![simple_module(vec![func("reset", vec![], None)])]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("void reset()"),
        "missing void function: {dart}"
    );
    assert!(
        dart.contains("Void Function("),
        "missing Void native return: {dart}"
    );
}

#[test]
fn string_function() {
    let api = make_api(vec![Module {
        name: "text".into(),
        functions: vec![func(
            "echo",
            vec![param("msg", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("String echo(String msg)"),
        "missing signature: {dart}"
    );
    assert!(
        dart.contains("toNativeUtf8()"),
        "missing toNativeUtf8: {dart}"
    );
    assert!(
        dart.contains("result.toDartString()"),
        "missing toDartString: {dart}"
    );
    assert!(
        dart.contains("calloc.free(msgPtr)"),
        "missing free for string: {dart}"
    );
    // The returned `const char*` is owned by the caller: copy first,
    // then release it through the runtime.
    assert!(
        dart.contains("final value = result.toDartString();\n    _weaveffiFreeString(result);"),
        "returned string must be copied then freed: {dart}"
    );
    assert!(
        dart.contains("'weaveffi_free_string'"),
        "missing weaveffi_free_string lookup: {dart}"
    );
}

#[test]
fn bool_function() {
    let api = make_api(vec![simple_module(vec![func(
        "is_valid",
        vec![param("flag", TypeRef::Bool)],
        Some(TypeRef::Bool),
    )])]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("bool isValid(bool flag)"),
        "missing signature: {dart}"
    );
    // A C `bool` crosses as the one-byte dart:ffi `Bool`, so the wrapper
    // passes and returns Dart bools without integer conversions.
    assert!(
        dart.contains("Bool Function(Bool, Pointer<_WeaveFFIError>)"),
        "missing Bool native signature: {dart}"
    );
    assert!(
        dart.contains("bool Function(bool, Pointer<_WeaveFFIError>)"),
        "missing bool dart signature: {dart}"
    );
    assert!(
        !dart.contains("flag ? 1 : 0") && !dart.contains("result != 0;"),
        "bool must not round-trip through ints: {dart}"
    );
}

#[test]
fn async_function() {
    let api = make_api(vec![simple_module(vec![Function {
        r#async: true,
        ..func(
            "fetch_data",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::StringUtf8),
        )
    }])]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("import 'dart:async'"),
        "missing dart:async import: {dart}"
    );
    assert!(
        dart.contains("Future<String> fetchData(int id)"),
        "missing async wrapper: {dart}"
    );
    assert!(
        dart.contains("NativeCallable<_NativeAsyncCb_weaveffi_math_fetch_data>.listener"),
        "missing NativeCallable.listener: {dart}"
    );
    assert!(
        dart.contains("weaveffi_math_fetch_data_async"),
        "must call the _async C symbol: {dart}"
    );
}

/// `NativeCallable.listener` allocates a native trampoline that pins the
/// Dart closure across the C boundary. It must be matched by exactly one
/// `callable.close()` on every exit path so the trampoline is freed when
/// the future resolves.
#[test]
fn dart_async_pins_callback_for_lifetime() {
    let api = make_api(vec![simple_module(vec![Function {
        r#async: true,
        ..func(
            "fetch_data",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::StringUtf8),
        )
    }])]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    let pin_count = dart.matches(".listener(").count();
    let unpin_count = dart.matches("callable.close()").count();
    assert_eq!(
        pin_count, 1,
        "expected one NativeCallable.listener per async fn, got {pin_count}: {dart}"
    );
    // Two close sites per fn: callback finally, and try/catch around _ffiCall.
    assert_eq!(
        unpin_count, 2,
        "expected callable.close() in callback finally and synchronous catch (2 total), got {unpin_count}: {dart}"
    );
}

#[test]
fn record_return_decodes_buffer() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![func(
            "get_contact",
            vec![param("id", TypeRef::Handle)],
            Some(TypeRef::Record("Contact".into())),
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Contact getContact(int id)"),
        "missing signature: {dart}"
    );
    // The buffered return is `Pointer<Uint8>` plus an `out_len` slot.
    assert!(
        dart.contains("Pointer<Uint8> Function(Int64, Pointer<Size>, Pointer<_WeaveFFIError>)"),
        "missing buffered return typedef: {dart}"
    );
    assert!(
        dart.contains("final outLen = calloc<Size>();"),
        "missing out_len alloc: {dart}"
    );
    // Copy, free the producer's buffer, decode, and reject trailing bytes.
    assert!(
        dart.contains("final data = _copyNativeBytes(result, n);"),
        "missing copy: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "buffered return must be freed after copying: {dart}"
    );
    assert!(
        dart.contains("final value = _unpackContact(reader);"),
        "missing decode: {dart}"
    );
    assert!(
        dart.contains("reader.expectEnd();"),
        "missing trailing-bytes check: {dart}"
    );
}

/// A buffered parameter is encoded, staged into native memory, passed as
/// a borrowed (ptr, len) pair, and freed by the caller afterwards.
#[test]
fn record_param_staged_and_freed() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![func(
            "save",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            None,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("final contactWriter = _BufferWriter();")
            && dart.contains("_packContact(contactWriter, contact);"),
        "missing param encode: {dart}"
    );
    assert!(
        dart.contains("final contactBuf = contactWriter.takeBytes();")
            && dart.contains("final contactPtr = _stageBytes(contactBuf);"),
        "missing native staging: {dart}"
    );
    assert!(
        dart.contains("_weaveffiContactsSave(contactPtr, contactBuf.length, err);"),
        "missing (ptr, len) call: {dart}"
    );
    assert!(
        dart.contains("calloc.free(contactPtr);"),
        "staged buffer must be freed by the caller: {dart}"
    );
    // The callee borrows the encoding; the wrapper never routes a
    // parameter through the runtime frees.
    assert!(
        !dart.contains("_weaveffiFreeBytes(contactPtr"),
        "borrowed param must not be runtime-freed: {dart}"
    );
}

#[test]
fn handle_uses_int64() {
    let api = make_api(vec![simple_module(vec![func(
        "create",
        vec![],
        Some(TypeRef::Handle),
    )])]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Int64 Function("),
        "missing Int64 for Handle: {dart}"
    );
}

#[test]
fn dart_generates_pubspec() {
    let api = make_api(vec![simple_module(vec![])]);
    let tmp = std::env::temp_dir().join("weaveffi_test_dart_pubspec");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DartGenerator
        .generate(&api, out_dir, &DartConfig::default())
        .unwrap();

    let pubspec_path = tmp.join("dart/pubspec.yaml");
    assert!(pubspec_path.exists(), "pubspec.yaml should exist");
    let pubspec = std::fs::read_to_string(&pubspec_path).unwrap();
    assert!(
        pubspec.contains("name: weaveffi"),
        "missing name: {pubspec}"
    );
    assert!(
        pubspec.contains("version: 0.1.0"),
        "missing version: {pubspec}"
    );
    assert!(
        pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
        "missing sdk constraint: {pubspec}"
    );
    assert!(
        pubspec.contains("ffi: ^2.0.0"),
        "missing ffi dependency: {pubspec}"
    );

    let readme_path = tmp.join("dart/README.md");
    assert!(readme_path.exists(), "README.md should exist");
    let readme = std::fs::read_to_string(&readme_path).unwrap();
    assert!(
        readme.contains("dart:ffi"),
        "README should mention dart:ffi: {readme}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_dart_with_optionals() {
    let api = make_api(vec![Module {
        name: "users".into(),
        functions: vec![func(
            "find_user",
            vec![param("id", TypeRef::I64)],
            Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("String? findUser(int id)"),
        "missing optional return type: {dart}"
    );
    // An optional is buffered: a flag byte, then the value when present.
    assert!(
        dart.contains("final value = (reader.readOptionFlag() ? reader.readString() : null);"),
        "missing optional decode: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "optional return buffer must be freed: {dart}"
    );
}

#[test]
fn generate_dart_with_lists() {
    let api = make_api(vec![Module {
        name: "data".into(),
        functions: vec![func(
            "get_scores",
            vec![param("items", TypeRef::List(Box::new(TypeRef::I32)))],
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("List<String> getScores(List<int> items)"),
        "missing list signature: {dart}"
    );
    // The list input is one serialized buffer: count then elements.
    assert!(
        dart.contains("itemsWriter.writeLength(t0.length);")
            && dart.contains("itemsWriter.writeInt32(t1);"),
        "missing list encode: {dart}"
    );
    assert!(
        dart.contains("_weaveffiDataGetScores(itemsPtr, itemsBuf.length, outLen, err)"),
        "missing (ptr, len) call with out_len: {dart}"
    );
    // The list return decodes count then elements from one buffer.
    assert!(
        dart.contains(
            "final value = List<String>.generate(reader.readLength(), (_) => reader.readString());"
        ),
        "missing list decode: {dart}"
    );
}

#[test]
fn generate_dart_with_maps() {
    let api = make_api(vec![Module {
        name: "cache".into(),
        functions: vec![func(
            "get_entries",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Map<String, int> getEntries()"),
        "missing map return type: {dart}"
    );
    // A map is one buffer: count then alternating key, value.
    assert!(
        dart.contains(
            "<String, int>{ for (var i = reader.readLength(); i > 0; i--) reader.readString(): reader.readInt32() }"
        ),
        "missing map decode: {dart}"
    );
    assert!(
        !dart.contains("outKeys") && !dart.contains("outValues"),
        "parallel key/value arrays are gone: {dart}"
    );
}

#[test]
fn generate_dart_with_typed_handle() {
    let api = make_api(vec![Module {
        name: "sessions".into(),
        functions: vec![
            func(
                "create_session",
                vec![param("name", TypeRef::StringUtf8)],
                Some(TypeRef::TypedHandle("Session".into())),
            ),
            func(
                "close_session",
                vec![param("session", TypeRef::TypedHandle("Session".into()))],
                None,
            ),
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Session createSession(String name)"),
        "missing typed handle return: {dart}"
    );
    assert!(
        dart.contains("Session._(result)"),
        "missing typed handle wrapping: {dart}"
    );
    assert!(
        dart.contains("void closeSession(Session session)"),
        "missing typed handle param: {dart}"
    );
    assert!(
        dart.contains("session._handle"),
        "missing _handle access for typed handle param: {dart}"
    );
}

#[test]
fn generate_dart_full_contacts() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![
            func(
                "create_contact",
                vec![
                    param("first_name", TypeRef::StringUtf8),
                    param("last_name", TypeRef::StringUtf8),
                    param("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    param("contact_type", TypeRef::Enum("ContactType".into())),
                ],
                Some(TypeRef::Handle),
            ),
            func(
                "get_contact",
                vec![param("id", TypeRef::Handle)],
                Some(TypeRef::Record("Contact".into())),
            ),
            func(
                "list_contacts",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
            ),
            func(
                "delete_contact",
                vec![param("id", TypeRef::Handle)],
                Some(TypeRef::Bool),
            ),
            func("count_contacts", vec![], Some(TypeRef::I32)),
        ],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: Some("A contact record".into()),
            fields: vec![
                field("id", TypeRef::I64),
                field("first_name", TypeRef::StringUtf8),
                field("last_name", TypeRef::StringUtf8),
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                field("contact_type", TypeRef::Enum("ContactType".into())),
            ],
        }],
        enums: vec![EnumDef {
            name: "ContactType".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Personal".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Work".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Other".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    assert!(
        dart.contains("enum ContactType {"),
        "missing ContactType enum: {dart}"
    );
    assert!(
        dart.contains("personal(0)"),
        "missing personal variant: {dart}"
    );
    assert!(dart.contains("work(1)"), "missing work variant: {dart}");
    assert!(dart.contains("other(2)"), "missing other variant: {dart}");

    assert!(
        dart.contains("class Contact {"),
        "missing Contact class: {dart}"
    );
    assert!(
        dart.contains("/// A contact record"),
        "missing doc comment: {dart}"
    );
    assert!(
        dart.contains("final int id;")
            && dart.contains("final String firstName;")
            && dart.contains("final String? email;")
            && dart.contains("final ContactType contactType;"),
        "missing typed final fields: {dart}"
    );
    // The enum field crosses the buffer as its i32 discriminant.
    assert!(
        dart.contains("w.writeInt32(v.contactType.value);")
            && dart.contains("contactType: ContactType.fromValue(r.readInt32()),"),
        "missing enum field encode/decode: {dart}"
    );

    assert!(
        dart.contains("int createContact("),
        "missing createContact: {dart}"
    );
    assert!(
        dart.contains("Contact getContact(int id)"),
        "missing getContact: {dart}"
    );
    assert!(
        dart.contains("List<Contact> listContacts()"),
        "missing listContacts: {dart}"
    );
    assert!(
        dart.contains("(_) => _unpackContact(reader)"),
        "list of records must decode elements: {dart}"
    );
    assert!(
        dart.contains("bool deleteContact(int id)"),
        "missing deleteContact: {dart}"
    );
    assert!(
        dart.contains("int countContacts()"),
        "missing countContacts: {dart}"
    );
}

#[test]
fn dart_custom_package_name() {
    let api = make_api(vec![simple_module(vec![])]);
    let tmp = std::env::temp_dir().join("weaveffi_test_dart_custom_pkg");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    let config = DartConfig {
        package_name: Some("my_custom_dart".into()),
        ..DartConfig::default()
    };
    DartGenerator.generate(&api, out_dir, &config).unwrap();

    let pubspec = std::fs::read_to_string(tmp.join("dart/pubspec.yaml")).unwrap();
    assert!(
        pubspec.contains("name: my_custom_dart"),
        "pubspec should use custom package name: {pubspec}"
    );
    assert!(
        !pubspec.contains("name: weaveffi"),
        "pubspec should not use default name: {pubspec}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dart_no_double_free_on_error() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        functions: vec![func(
            "find_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Record("Contact".into())),
        )],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    assert!(
        !dart.contains("weaveffi_free_string(namePtr"),
        "borrowed string param must not be freed via weaveffi_free_string: {dart}"
    );

    let fn_start = dart
        .find("Contact findContact(")
        .expect("findContact wrapper");
    let fn_body = &dart[fn_start..];

    let err_check = fn_body
        .find("_checkError(err)")
        .expect("_checkError in findContact");
    let decode = fn_body
        .find("_unpackContact(reader)")
        .expect("decode in findContact");
    assert!(
        err_check < decode,
        "error must be checked before decoding the return buffer: {dart}"
    );
}

#[test]
fn dart_null_check_on_optional_return() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![func(
            "find_contact",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    assert!(
        dart.contains("Contact? findContact(int id)"),
        "missing optional record signature: {dart}"
    );
    // The option is a flag byte inside the buffer, not a null pointer.
    assert!(
        dart.contains("final value = (reader.readOptionFlag() ? _unpackContact(reader) : null);"),
        "optional record must decode the flag then the value: {dart}"
    );
}

fn doc_api() -> Api {
    make_api(vec![Module {
        name: "docs".into(),
        functions: vec![Function {
            doc: Some("Performs a thing.".into()),
            ..func(
                "do_thing",
                vec![param("x", TypeRef::I32)],
                Some(TypeRef::I32),
            )
        }],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
                default: None,
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
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn dart_emits_doc_on_function() {
    let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
    assert!(dart.contains("/// Performs a thing."), "{dart}");
}

#[test]
fn dart_emits_doc_on_struct() {
    let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
    assert!(dart.contains("/// An item we track."), "{dart}");
}

#[test]
fn dart_emits_doc_on_enum_variant() {
    let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
    assert!(dart.contains("/// Kind of item."), "{dart}");
    assert!(dart.contains("/// A small one"), "{dart}");
}

#[test]
fn dart_emits_doc_on_field() {
    let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("/// Stable id\n  final int id;"),
        "field doc should sit on the final field: {dart}"
    );
}

/// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
/// f64 payload, two f32 payloads, and a (string, u8) payload, plus a plain
/// sibling enum and functions that take/return the rich enum by value.
fn rich_enum_api() -> Api {
    make_api(vec![Module {
        name: "shapes".into(),
        functions: vec![
            func(
                "describe",
                vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::StringUtf8),
            ),
            func(
                "scale",
                vec![
                    param("shape", TypeRef::RichEnum("Shape".into())),
                    param("factor", TypeRef::F64),
                ],
                Some(TypeRef::RichEnum("Shape".into())),
            ),
        ],
        structs: vec![],
        enums: vec![
            EnumDef {
                name: "Shape".into(),
                doc: Some("An algebraic shape".into()),
                variants: vec![
                    EnumVariant {
                        name: "Empty".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Circle".into(),
                        value: 1,
                        doc: None,
                        fields: vec![StructField {
                            name: "radius".into(),
                            ty: TypeRef::F64,
                            doc: Some("Radius in points".into()),
                            default: None,
                        }],
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
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn rich_enum_is_sealed_hierarchy() {
    let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
    // The rich enum must NOT be a plain Dart `enum`...
    assert!(
        !dart.contains("enum Shape {"),
        "rich enum must not render as a plain enum: {dart}"
    );
    // ...but a sealed base class with one subclass per variant.
    assert!(
        dart.contains("sealed class Shape {"),
        "missing sealed base: {dart}"
    );
    assert!(
        dart.contains("class ShapeEmpty extends Shape {}"),
        "missing unit variant subclass: {dart}"
    );
    assert!(
        dart.contains("class ShapeCircle extends Shape {"),
        "missing circle subclass: {dart}"
    );
    assert!(
        dart.contains("final double radius;") && dart.contains("ShapeCircle(this.radius);"),
        "variant fields must be final constructor fields: {dart}"
    );
    assert!(
        dart.contains("ShapeLabeled(this.label, this.count);"),
        "multi-field variant constructor: {dart}"
    );
    // Rich enums declare no C symbols: no handle, no dispose, no
    // per-variant constructors or getters.
    assert!(
        !dart.contains("Shape._(") && !dart.contains("weaveffi_shapes_Shape_"),
        "rich enum must not bind C symbols: {dart}"
    );
    // Carries the per-variant field doc onto the final field.
    assert!(
        dart.contains("/// Radius in points"),
        "variant field doc should be emitted: {dart}"
    );
    // A plain sibling enum still renders as a plain Dart enum.
    assert!(
        dart.contains("enum Channel {"),
        "plain sibling enum should still render as an enum: {dart}"
    );
}

#[test]
fn rich_enum_pack_writes_tag_then_fields() {
    let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("void _packShape(_BufferWriter w, Shape v) {"),
        "missing pack helper: {dart}"
    );
    assert!(
        dart.contains("case ShapeEmpty():") && dart.contains("w.writeInt32(0);"),
        "unit variant must write only its tag: {dart}"
    );
    assert!(
        dart.contains("case final ShapeCircle t0:")
            && dart.contains("w.writeInt32(1);")
            && dart.contains("w.writeFloat64(t0.radius);"),
        "circle must write tag then f64 radius: {dart}"
    );
    assert!(
        dart.contains("w.writeFloat32(t1.width);") && dart.contains("w.writeFloat32(t1.height);"),
        "rectangle must write both f32 fields in order: {dart}"
    );
    assert!(
        dart.contains("w.writeString(t2.label);") && dart.contains("w.writeUint8(t2.count);"),
        "labeled must write string then u8: {dart}"
    );
}

#[test]
fn rich_enum_unpack_switches_on_tag() {
    let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Shape _unpackShape(_BufferReader r) {"),
        "missing unpack helper: {dart}"
    );
    assert!(
        dart.contains("final tag = r.readInt32();"),
        "missing tag read: {dart}"
    );
    assert!(
        dart.contains("return ShapeEmpty();"),
        "missing unit variant arm: {dart}"
    );
    assert!(
        dart.contains("return ShapeCircle(r.readFloat64());"),
        "missing circle arm: {dart}"
    );
    assert!(
        dart.contains("return ShapeRectangle(r.readFloat32(), r.readFloat32());"),
        "missing rectangle arm: {dart}"
    );
    assert!(
        dart.contains("return ShapeLabeled(r.readString(), r.readUint8());"),
        "missing labeled arm: {dart}"
    );
    // An unknown tag is a contract violation, not a silent default.
    assert!(
        dart.contains("_bufferError('unknown Shape tag $tag');"),
        "missing unknown-tag rejection: {dart}"
    );
}

#[test]
fn rich_enum_functions_marshal_buffers() {
    let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("String describe(Shape shape)"),
        "missing describe signature: {dart}"
    );
    assert!(
        dart.contains("Shape scale(Shape shape, double factor)"),
        "missing scale signature: {dart}"
    );
    // A rich-enum argument is encoded and staged as a (ptr, len) pair...
    assert!(
        dart.contains("_packShape(shapeWriter, shape);")
            && dart.contains("final shapePtr = _stageBytes(shapeBuf);"),
        "rich-enum argument must be encoded and staged: {dart}"
    );
    // ...and a rich-enum return decodes then frees the buffer.
    assert!(
        dart.contains("final value = _unpackShape(reader);"),
        "rich-enum return must decode: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "rich-enum return buffer must be freed: {dart}"
    );
}

/// A `kv` module with a declared error domain and a `Store` interface
/// exercising every member kind: a plain constructor named `new`, a
/// throwing named constructor, throwing and non-throwing methods, an
/// async throwing method, an iterator method, and a static.
fn store_api() -> Api {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};
    fn f(
        name: &str,
        params: Vec<Param>,
        returns: Option<TypeRef>,
        throws: bool,
        is_async: bool,
    ) -> Function {
        Function {
            throws,
            r#async: is_async,
            ..func(name, params, returns)
        }
    }
    make_api(vec![Module {
        name: "kv".into(),
        functions: vec![f(
            "inspect",
            vec![param("store", TypeRef::Interface("Store".into()))],
            Some(TypeRef::I64),
            false,
            false,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store.".into()),
            constructors: vec![
                f(
                    "new",
                    vec![param("capacity", TypeRef::I64)],
                    None,
                    false,
                    false,
                ),
                f(
                    "open",
                    vec![param("path", TypeRef::StringUtf8)],
                    None,
                    true,
                    false,
                ),
            ],
            methods: vec![
                f(
                    "put",
                    vec![
                        param("key", TypeRef::StringUtf8),
                        param("value", TypeRef::StringUtf8),
                    ],
                    None,
                    true,
                    false,
                ),
                f("count", vec![], Some(TypeRef::I64), false, false),
                f("compact", vec![], Some(TypeRef::I64), true, true),
                f(
                    "list_keys",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                    true,
                    false,
                ),
            ],
            statics: vec![f(
                "default_capacity",
                vec![],
                Some(TypeRef::I64),
                false,
                false,
            )],
        }],
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
                    name: "IoError".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }])
}

#[test]
fn typed_exception_rendering() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    // The domain exception extends the generic brand exception.
    assert!(
        dart.contains("class KvException extends WeaveFFIException {"),
        "missing domain exception: {dart}"
    );
    assert!(
        dart.contains("KvException(super.code, super.message);"),
        "domain exception must forward code and message: {dart}"
    );
    // One subclass per code, preloaded with its stable code and message.
    assert!(
        dart.contains("class KeyNotFoundException extends KvException {"),
        "missing per-code subclass: {dart}"
    );
    assert!(
        dart.contains(
            "KeyNotFoundException([String message = 'key not found']) : super(1001, message);"
        ),
        "per-code subclass must carry its code and default message: {dart}"
    );
    // A code already named `*Error` swaps the suffix rather than stacking.
    assert!(
        dart.contains("class IoException extends KvException {")
            && !dart.contains("IoErrorException"),
        "code exception must swap the Error suffix: {dart}"
    );
    // The mapper covers each code, receives the payload buffer, and falls
    // back to the generic exception.
    assert!(
        dart.contains(
            "WeaveFFIException _mapKvException(int code, String message, Uint8List payload) {"
        ),
        "missing domain mapper: {dart}"
    );
    assert!(
        dart.contains("case 1001:") && dart.contains("return KeyNotFoundException(message);"),
        "mapper must build the per-code subclass: {dart}"
    );
    assert!(
        dart.contains("default:") && dart.contains("return WeaveFFIException(code, message);"),
        "mapper must fall back to the generic exception: {dart}"
    );
    // The per-domain check helper copies the payload before clearing.
    assert!(
        dart.contains("void _checkKvException(Pointer<_WeaveFFIError> err) {")
            && dart.contains(
                "final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);"
            )
            && dart.contains("throw _mapKvException(code, msg, payload);"),
        "missing domain check helper: {dart}"
    );
}

/// A code that declares payload fields decodes them from the error's
/// payload buffer and exposes them as typed properties on the exception.
#[test]
fn error_payload_fields_decode_onto_exception() {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![Function {
            throws: true,
            ..func(
                "get",
                vec![param("key", TypeRef::StringUtf8)],
                Some(TypeRef::I64),
            )
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: None,
                    fields: vec![
                        field("key", TypeRef::StringUtf8),
                        field("attempts", TypeRef::I32),
                    ],
                },
                ErrorCode {
                    name: "IoError".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "kv.yml");
    // The exception carries the decoded payload as final typed fields.
    assert!(
        dart.contains("class KeyNotFoundException extends KvException {")
            && dart.contains("final String key;")
            && dart.contains("final int attempts;"),
        "missing payload fields on the exception: {dart}"
    );
    assert!(
        dart.contains(
            "KeyNotFoundException(this.key, this.attempts, [String message = 'key not found']) : super(1001, message);"
        ),
        "missing payload-aware constructor: {dart}"
    );
    // The mapper decodes the payload in declaration (wire) order and
    // rejects trailing bytes.
    assert!(
        dart.contains("final r = _BufferReader(payload);")
            && dart.contains("final v0 = r.readString();")
            && dart.contains("final v1 = r.readInt32();")
            && dart.contains("r.expectEnd();")
            && dart.contains("return KeyNotFoundException(v0, v1, message);"),
        "mapper must decode payload fields in order: {dart}"
    );
    // A code without fields still maps directly.
    assert!(
        dart.contains("return IoException(message);"),
        "plain code must map without payload decoding: {dart}"
    );
}

#[test]
fn interface_emits_wrapper_class_with_dispose() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("/// A key-value store.\nclass Store {"),
        "missing documented interface class: {dart}"
    );
    assert!(
        dart.contains("final Pointer<Void> _handle;") && dart.contains("Store._(this._handle);"),
        "missing opaque handle plumbing: {dart}"
    );
    let dispose = dart
        .find("class Store {")
        .map(|i| &dart[i..])
        .expect("class body");
    assert!(
        dispose.contains("void dispose() {\n    _weaveffiKvStoreDestroy(_handle);"),
        "dispose must call the interface destroy symbol: {dart}"
    );
    assert!(
        dart.contains("'weaveffi_kv_Store_destroy'"),
        "destroy lookup must bind the C symbol: {dart}"
    );
}

#[test]
fn interface_ctor_new_is_unnamed_factory() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("factory Store(int capacity) {"),
        "missing unnamed factory for ctor `new`: {dart}"
    );
    let body = &dart[dart.find("factory Store(int capacity)").expect("ctor body")..];
    assert!(
        body.contains("_weaveffiKvStoreNew(capacity, err)"),
        "ctor must call its member symbol: {dart}"
    );
    // Non-throwing ctor still traps through the generic check.
    assert!(
        body.contains("_checkError(err);"),
        "plain ctor must use the generic check: {dart}"
    );
    assert!(
        body.contains("return Store._(result);"),
        "ctor must adopt the owned handle: {dart}"
    );
}

#[test]
fn interface_secondary_ctor_is_named_factory() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("factory Store.open(String path) {"),
        "missing named factory: {dart}"
    );
    let body = &dart[dart.find("factory Store.open(").expect("open body")..];
    assert!(
        body.contains("_weaveffiKvStoreOpen(pathPtr, err)"),
        "named factory must call its member symbol: {dart}"
    );
    assert!(
        body.contains("_checkKvException(err);"),
        "throwing factory must use the domain check: {dart}"
    );
    assert!(
        body.contains("return Store._(result);"),
        "named factory must adopt the owned handle: {dart}"
    );
    // The throwing ctor documents the thrown domain exception.
    assert!(
        dart.contains("/// Throws [KvException] on domain errors.\n  factory Store.open("),
        "throwing ctor must note the thrown type: {dart}"
    );
}

#[test]
fn interface_methods_pass_self_handle() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    // Throwing instance method: `_handle` leads the C argument list.
    assert!(
        dart.contains("void put(String key, String value) {"),
        "missing instance method: {dart}"
    );
    assert!(
        dart.contains("_weaveffiKvStorePut(_handle, keyPtr, valuePtr, err);"),
        "method must pass _handle as the leading argument: {dart}"
    );
    let put_body = &dart[dart.find("void put(").expect("put body")..];
    assert!(
        put_body.contains("_checkKvException(err);"),
        "throwing method must use the domain check: {dart}"
    );
    // Non-throwing method uses the generic check.
    let count_body = &dart[dart.find("int count()").expect("count body")..];
    assert!(
        count_body.contains("_weaveffiKvStoreCount(_handle, err)")
            && count_body.contains("_checkError(err);"),
        "plain method must call with _handle and check generically: {dart}"
    );
}

#[test]
fn interface_async_method_maps_typed_error() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("Future<int> compact() {"),
        "missing async method: {dart}"
    );
    assert!(
        dart.contains("_weaveffiKvStoreCompactAsync(_handle, callable.nativeFunction, nullptr);"),
        "async launcher must lead with _handle: {dart}"
    );
    // The typed completion copies the payload inside the borrow window
    // and completes with the mapped domain exception.
    assert!(
        dart.contains("completer.completeError(_mapKvException(code, msg, payload));"),
        "async throwing method must complete with the typed exception: {dart}"
    );
}

#[test]
fn interface_iterator_method_checks_domain() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("Iterable<String> listKeys() sync* {"),
        "missing lazy iterator method: {dart}"
    );
    assert!(
        dart.contains("_weaveffiKvStoreListKeys(_handle, err)"),
        "iterator launch must lead with _handle: {dart}"
    );
    let body = &dart[dart
        .find("Iterable<String> listKeys()")
        .expect("listKeys body")..];
    assert!(
        body.contains("_checkKvException(err);"),
        "throwing iterator must route launch and next through the domain check: {dart}"
    );
}

/// The `iter<T>` wrapper must be a lazy `sync*` generator: one producer
/// `next` call per yielded element, no hidden drain into a list, and a
/// `try`/`finally` that destroys the handle exactly once (nulling it) on
/// exhaustion, error, or generator teardown.
#[test]
fn iterator_wrapper_is_lazy_sync_star() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    let body = &dart[dart
        .find("Iterable<String> listKeys() sync* {")
        .expect("sync* wrapper")..];
    let body = &body[..body.find("\n  }").expect("member end")];
    // One `next` per consumer step, yielded straight out of the loop.
    assert!(
        body.contains("while (_weaveffiKvStoreListKeysIteratorNext(iter, outItem, err) != 0) {"),
        "missing per-element next loop: {body}"
    );
    assert!(body.contains("yield item;"), "missing yield: {body}");
    assert!(
        !body.contains(".add(") && !body.contains("return items;"),
        "iterator must not drain into a list: {body}"
    );
    // Destroy exactly once, guarded and nulled, from the finally block.
    assert!(body.contains("} finally {"), "missing finally: {body}");
    assert!(
        body.contains("if (iter != nullptr) {")
            && body.contains("_weaveffiKvStoreListKeysIteratorDestroy(iter);")
            && body.contains("iter = nullptr;"),
        "finally must destroy once and null the handle: {body}"
    );
    // String elements are copied then freed per ElemFree::String.
    assert!(
        body.contains("final item = itemPtr.toDartString();")
            && body.contains("_weaveffiFreeString(itemPtr);"),
        "string elements must be copied then freed: {body}"
    );
}

/// Abandoned iterations (a broken `for`, `first`, `take`) never resume a
/// `sync*` body, so its `finally` cannot run; the wrapper attaches a
/// `NativeFinalizer` backstop to a generator-local anchor and detaches it
/// before the eager destroy so double-destroy is impossible.
#[test]
fn iterator_wrapper_has_finalizer_backstop() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("final class _IteratorLifetime implements Finalizable {}"),
        "missing iterator lifetime anchor class: {dart}"
    );
    assert!(
        dart.contains("final _weaveffiKvStoreListKeysIteratorDestroyFinalizer = NativeFinalizer("),
        "missing NativeFinalizer over the destroy symbol: {dart}"
    );
    let body = &dart[dart
        .find("Iterable<String> listKeys() sync* {")
        .expect("sync* wrapper")..];
    let body = &body[..body.find("\n  }").expect("member end")];
    assert!(
        body.contains(
            "_weaveffiKvStoreListKeysIteratorDestroyFinalizer.attach(anchor, iter, detach: anchor);"
        ),
        "launch must attach the finalizer backstop: {body}"
    );
    assert!(
        body.contains("_weaveffiKvStoreListKeysIteratorDestroyFinalizer.detach(anchor);"),
        "eager destroy must detach the backstop first: {body}"
    );
}

/// A free function returning `iter<record>` decodes each producer buffer
/// then frees it with `weaveffi_free_bytes` (`ElemFree::Bytes`), and its
/// `_next` slot carries the extra `out_len` pointer.
#[test]
fn iterator_of_records_decodes_and_frees_elements() {
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![Function {
            doc: Some("Streams every entry.".into()),
            ..func(
                "entries",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
            )
        }],
        structs: vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![field("key", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "kv.yml");
    assert!(
        dart.contains("Iterable<Entry> entries() sync* {"),
        "missing record iterator wrapper: {dart}"
    );
    // The `_next` typedef carries `out_item` plus `out_len`.
    assert!(
        dart.contains(
            "Pointer<Void>, Pointer<Pointer<Uint8>>, Pointer<Size>, Pointer<_WeaveFFIError>"
        ),
        "missing buffered next slots: {dart}"
    );
    assert!(
        dart.contains("final outLen = calloc<Size>();"),
        "missing out_len alloc: {dart}"
    );
    // Each element is copied, freed with weaveffi_free_bytes, and decoded.
    assert!(
        dart.contains("final itemData = _copyNativeBytes(itemPtr, itemLen);")
            && dart.contains("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);")
            && dart.contains("final item = _unpackEntry(itemReader);")
            && dart.contains("itemReader.expectEnd();"),
        "record elements must be decoded then freed: {dart}"
    );
    // Non-throwing: launch and next errors trap via the generic check.
    let body = &dart[dart.find("Iterable<Entry> entries()").expect("body")..];
    assert!(
        body.contains("_checkError(err);"),
        "trap-strategy iterator must use the generic check: {dart}"
    );
    // The generated doc states the streaming contract.
    assert!(
        dart.contains("/// Returns a lazy [Iterable]:"),
        "missing streaming doc: {dart}"
    );
    // Record elements are plain values now; no dispose note applies.
    assert!(
        !dart.contains("/// Each yielded element is owned by the caller:"),
        "record elements carry no dispose obligation: {dart}"
    );
}

#[test]
fn interface_static_is_static_method() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("static int defaultCapacity() {"),
        "missing static method: {dart}"
    );
    let body = &dart[dart
        .find("static int defaultCapacity()")
        .expect("static body")..];
    assert!(
        body.contains("_weaveffiKvStoreDefaultCapacity(err)"),
        "static must call its member symbol without a self slot: {dart}"
    );
}

#[test]
fn interface_param_passes_borrowed_handle() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    // Free function taking the interface: the class is the Dart type and
    // the call borrows its handle without wrapping or disposing.
    assert!(
        dart.contains("int inspect(Store store) {"),
        "missing interface-typed param signature: {dart}"
    );
    assert!(
        dart.contains("_weaveffiKvInspect(store._handle, err)"),
        "interface param must pass ._handle: {dart}"
    );
}

#[test]
fn throws_split_on_free_functions() {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
    let api = make_api(vec![Module {
        name: "calc".into(),
        functions: vec![
            Function {
                throws: true,
                ..func(
                    "div",
                    vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                    Some(TypeRef::I32),
                )
            },
            func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            ),
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "CalcError".into(),
            codes: vec![ErrorCode {
                name: "DivisionByZero".into(),
                code: 1,
                message: "Division by zero".into(),
                doc: None,
                fields: vec![],
            }],
        }),
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "calc.yml");
    // throws: true routes the slot through the domain check and says so.
    let div_body = &dart[dart.find("int div(int a, int b)").expect("div body")..];
    assert!(
        div_body.contains("_checkCalcException(err);"),
        "throwing fn must use the domain check: {dart}"
    );
    assert!(
        dart.contains("/// Throws [CalcException] on domain errors.\nint div(int a, int b) {"),
        "throwing fn must note the thrown type: {dart}"
    );
    // throws: false keeps the generic check for panics and marshalling.
    let add_body = &dart[dart.find("int add(int a, int b)").expect("add body")..];
    assert!(
        add_body.contains("_checkError(err);"),
        "plain fn must check generically: {dart}"
    );
    assert!(
        !add_body[..add_body.find('}').unwrap_or(add_body.len())].contains("_checkCalcException"),
        "plain fn must not use the domain check: {dart}"
    );
}

#[test]
fn strip_module_prefix_defaults_to_true() {
    assert!(
        DartConfig::default().strip_module_prefix,
        "stripping must be the default"
    );
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");
    assert!(
        dart.contains("int inspect(Store store) {") && !dart.contains("int kvInspect("),
        "default naming must strip the module prefix: {dart}"
    );
}

/// Mirrors the `cli_dart.rs` expectations for `samples/contacts` by
/// rendering the sample directly; kept here because the CLI binary cannot
/// build while other generator crates are mid-overhaul.
#[test]
fn contacts_sample_renders_interface_and_domain() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join("../../samples/contacts/contacts.yml");
    let src = std::fs::read_to_string(path).expect("contacts sample readable");
    let mut api = weaveffi_ir::parse::parse_api_str(&src, "yaml").expect("contacts sample parses");
    // Generators run strictly post-resolution: rewrite every parsed
    // `Named` reference into its resolved kind first, as the CLI does.
    weaveffi_core::validate::resolve_type_refs(&mut api);
    let dart = render_dart_module(&api, "weaveffi", "contacts.yml");
    assert!(
        dart.contains("enum ContactType {"),
        "missing ContactType enum: {dart}"
    );
    assert!(dart.contains("class Contact {"), "missing Contact: {dart}");
    assert!(
        dart.contains("void _packContact(_BufferWriter w, Contact v) {")
            && dart.contains("Contact _unpackContact(_BufferReader r) {"),
        "missing Contact buffer helpers: {dart}"
    );
    assert!(
        dart.contains("class ContactBook {") && dart.contains("factory ContactBook() {"),
        "missing ContactBook interface: {dart}"
    );
    assert!(
        dart.contains("class ContactsException extends WeaveFFIException {"),
        "missing ContactsException: {dart}"
    );
    assert!(
        dart.contains("weaveffi_contacts_ContactBook_add"),
        "missing ContactBook add member symbol: {dart}"
    );
    // Records declare no C symbols in the new ABI.
    assert!(
        !dart.contains("weaveffi_contacts_Contact_"),
        "record C symbols must be gone: {dart}"
    );
}

/// One-function module helper for the ownership-audit tests below.
fn returning(name: &str, returns: TypeRef) -> Api {
    make_api(vec![simple_module(vec![func(name, vec![], Some(returns))])])
}

#[test]
fn bytes_return_copies_then_frees_buffer() {
    let dart = render_dart_module(
        &returning("blob", TypeRef::Bytes),
        "weaveffi",
        "weaveffi.yml",
    );
    assert!(
        dart.contains("final bytes = List<int>.generate(n, (i) => result[i]);"),
        "bytes must be copied: {dart}"
    );
    assert!(
        dart.contains("_weaveffiFreeBytes(result, n);"),
        "bytes buffer must be freed after copying: {dart}"
    );
    assert!(
        dart.contains("'weaveffi_free_bytes'"),
        "missing weaveffi_free_bytes lookup: {dart}"
    );
}

#[test]
fn string_list_return_decodes_one_buffer() {
    let dart = render_dart_module(
        &returning("names", TypeRef::List(Box::new(TypeRef::StringUtf8))),
        "weaveffi",
        "weaveffi.yml",
    );
    // One producer buffer holding count + length-prefixed strings; no
    // per-element C strings exist any more.
    assert!(
        dart.contains(
            "final value = List<String>.generate(reader.readLength(), (_) => reader.readString());"
        ),
        "missing element decode: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "list buffer must be freed once: {dart}"
    );
    assert!(
        !dart.contains("_weaveffiFreeString(arr[i]);"),
        "no per-element string frees remain: {dart}"
    );
}

#[test]
fn map_return_decodes_one_buffer() {
    let dart = render_dart_module(
        &returning(
            "tally",
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
        ),
        "weaveffi",
        "weaveffi.yml",
    );
    assert!(
        dart.contains(
            "<String, int>{ for (var i = reader.readLength(); i > 0; i--) reader.readString(): reader.readInt32() }"
        ),
        "missing map decode: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "map buffer must be freed once: {dart}"
    );
}

#[test]
fn optional_scalar_return_decodes_flag() {
    let dart = render_dart_module(
        &returning("level", TypeRef::Optional(Box::new(TypeRef::I64))),
        "weaveffi",
        "weaveffi.yml",
    );
    assert!(
        dart.contains("final value = (reader.readOptionFlag() ? reader.readInt64() : null);"),
        "boxed optionals are gone; the flag byte decides presence: {dart}"
    );
    assert!(
        dart.contains("if (result != nullptr) _weaveffiFreeBytes(result, n);"),
        "optional return buffer must be freed: {dart}"
    );
}

/// Async result buffers are borrowed for the callback's duration: the
/// wrapper decodes them inside the callback and never frees them.
#[test]
fn async_buffer_results_decode_and_never_free() {
    let api = make_api(vec![simple_module(vec![
        Function {
            r#async: true,
            ..func(
                "fetch_names",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            )
        },
        Function {
            r#async: true,
            ..func("fetch_blob", vec![], Some(TypeRef::Bytes))
        },
    ])]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("Future<List<String>> fetchNames()"),
        "missing async list wrapper: {dart}"
    );
    // The borrowed (ptr, len) pair is copied and decoded in the callback.
    assert!(
        dart.contains("final resultData = _copyNativeBytes(result, resultLen);")
            && dart.contains(
                "final value = List<String>.generate(resultReader.readLength(), (_) => resultReader.readString());"
            ),
        "async buffered result must be decoded inside the callback: {dart}"
    );
    assert!(
        dart.contains("completer.complete(_copyNativeBytes(result, resultLen));"),
        "async bytes result must be copied: {dart}"
    );
    // Borrowed: the callback must not release the producer's buffers.
    let cb = &dart[dart
        .find("Future<List<String>> fetchNames()")
        .expect("wrapper")..];
    let cb = &cb[..cb.find("\n}").expect("end")];
    assert!(
        !cb.contains("_weaveffiFree"),
        "async callback must never free borrowed result buffers: {cb}"
    );
}

/// A buffered async *input* is staged like a sync input and released only
/// when the future completes (or the launch throws).
#[test]
fn async_buffered_input_staged_until_completion() {
    let api = make_api(vec![Module {
        name: "jobs".into(),
        functions: vec![Function {
            r#async: true,
            ..func(
                "submit",
                vec![param("tags", TypeRef::List(Box::new(TypeRef::StringUtf8)))],
                Some(TypeRef::I64),
            )
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    assert!(
        dart.contains("final tagsPtr = _stageBytes(tagsBuf);"),
        "missing staged async input: {dart}"
    );
    assert!(
        dart.contains(
            "_weaveffiJobsSubmitAsync(tagsPtr, tagsBuf.length, callable.nativeFunction, nullptr);"
        ),
        "launcher must pass the (ptr, len) pair: {dart}"
    );
    assert!(
        dart.contains("return completer.future.whenComplete(() {")
            && dart.contains("calloc.free(tagsPtr);"),
        "staged input must be freed on completion: {dart}"
    );
}

/// Buffered callback/listener arguments are borrowed (ptr, len) pairs
/// valid only during the dispatch: the trampoline decodes them before
/// invoking the user's closure and never frees them.
#[test]
fn listener_buffered_argument_decoded_in_borrow_window() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Event".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "on_event".into(),
            params: vec![param("event", TypeRef::Record("Event".into()))],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "events".into(),
            event_callback: "on_event".into(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
    // The native callback typedef carries the (ptr, len) pair + context.
    assert!(
        dart.contains(
            "typedef _NativeCb_weaveffi_events_on_event_fn = Void Function(Pointer<Uint8>, Size, Pointer<Void>);"
        ),
        "missing buffered callback typedef: {dart}"
    );
    // The trampoline decodes inside the borrow window, then dispatches.
    assert!(
        dart.contains("(Pointer<Uint8> eventPtr, int eventLen, Pointer<Void> context) {"),
        "missing trampoline slots: {dart}"
    );
    assert!(
        dart.contains("final eventData = _copyNativeBytes(eventPtr, eventLen);")
            && dart.contains("final eventValue = _unpackEvent(eventReader);")
            && dart.contains("callback(eventValue);"),
        "trampoline must decode before invoking the user callback: {dart}"
    );
    // Borrowed: never freed by the consumer.
    assert!(
        !dart.contains("_weaveffiFreeBytes(eventPtr"),
        "borrowed callback argument must not be freed: {dart}"
    );
    // Register/unregister plumbing is unchanged.
    assert!(
        dart.contains("int registerEvents(void Function(Event event) callback) {")
            && dart.contains("void unregisterEvents(int id) {"),
        "missing listener wrappers: {dart}"
    );
}

#[test]
fn strip_module_prefix_can_be_disabled() {
    let api = store_api();
    let model = BindingModel::build(&api, "weaveffi");
    let config = DartConfig {
        prefix: Some("weaveffi".into()),
        input_basename: Some("kv.yml".into()),
        strip_module_prefix: false,
        ..DartConfig::default()
    };
    let dart = DartGenerator.render_dart_source(&api, &model, &config);
    assert!(
        dart.contains("int kvInspect(Store store) {"),
        "disabled stripping must keep the module prefix: {dart}"
    );
    // Interface members are namespaced by their class, never prefixed.
    assert!(
        dart.contains("factory Store.open(String path) {"),
        "interface members must not gain a module prefix: {dart}"
    );
}

/// IDL names that are Dart reserved words gain a trailing underscore at
/// every user-chosen identifier position: parameters, locals derived from
/// them, record fields (declaration, constructor, pack, and unpack), and
/// enum variant names. Non-reserved names pass through untouched.
#[test]
fn reserved_word_identifiers_are_escaped() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "kw".into(),
        functions: vec![func(
            "load",
            vec![
                param("class", TypeRef::StringUtf8),
                param("in", TypeRef::I32),
                param("normal", TypeRef::Bool),
            ],
            Some(TypeRef::I32),
        )],
        structs: vec![StructDef {
            name: "Config".into(),
            doc: None,
            fields: vec![
                field("class", TypeRef::StringUtf8),
                field("default", TypeRef::Optional(Box::new(TypeRef::I32))),
                field("normal", TypeRef::Bool),
            ],
        }],
        enums: vec![EnumDef {
            name: "Mode".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "New".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Existing".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![CallbackDef {
            name: "on_kw".into(),
            params: vec![param("switch", TypeRef::I32)],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "kw_events".into(),
            event_callback: "on_kw".into(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

    // Parameters escape in the wrapper signature; the derived staging local
    // keeps the escaped base.
    assert!(
        dart.contains("int load(String class_, int in_, bool normal) {"),
        "reserved parameter names must be escaped: {dart}"
    );
    assert!(
        dart.contains("final class_Ptr = class_.toNativeUtf8();"),
        "locals derived from an escaped name must reuse it: {dart}"
    );
    // Record fields escape at every position: declaration, constructor,
    // pack expression, and unpack key.
    assert!(
        dart.contains("final String class_;") && dart.contains("final int? default_;"),
        "reserved field names must be escaped: {dart}"
    );
    assert!(
        dart.contains("Config({required this.class_, this.default_, required this.normal});"),
        "constructor params must use the escaped names: {dart}"
    );
    assert!(
        dart.contains("w.writeString(v.class_);"),
        "pack helper must read the escaped field: {dart}"
    );
    assert!(
        dart.contains("class_: r.readString(),"),
        "unpack helper must name the escaped field: {dart}"
    );
    // Enum variant names escape too (`New` lower-camels to the reserved
    // `new`).
    assert!(
        dart.contains("new_(0),") && dart.contains("existing(1),"),
        "reserved variant names must be escaped: {dart}"
    );
    // Callback trampoline slots and the user-facing callback signature
    // escape reserved parameter names.
    assert!(
        dart.contains("void Function(int switch_) callback"),
        "callback signature must escape reserved names: {dart}"
    );
    assert!(
        !dart.contains(" switch,") && !dart.contains("(int switch)"),
        "no bare reserved word may survive as an identifier: {dart}"
    );
}

/// Domain error mapping covers exactly the declared (positive) codes:
/// every other code, in particular the reserved negative runtime range
/// (`-1` generic, `-2` panic, `-3` marshalling), falls through to the
/// generic `WeaveFFIException` on throwing paths, and non-throwing paths
/// trap through `_checkError`, which always throws the generic exception.
#[test]
fn runtime_error_codes_fall_through_to_generic() {
    let dart = render_dart_module(&store_api(), "weaveffi", "kv.yml");

    // The mapper switches only on the declared positive codes.
    let mapper_start = dart
        .find("WeaveFFIException _mapKvException(")
        .expect("domain mapper");
    let mapper = &dart[mapper_start..];
    let mapper = &mapper[..mapper.find("\n}").expect("mapper end")];
    assert!(
        mapper.contains("case 1001:") && mapper.contains("case 1004:"),
        "mapper must cover each declared code: {mapper}"
    );
    assert!(
        !mapper.contains("case -"),
        "no negative code may gain a typed case: {mapper}"
    );
    assert!(
        mapper.contains("default:\n      return WeaveFFIException(code, message);"),
        "undeclared codes must fall through to the generic exception: {mapper}"
    );

    // Non-throwing paths trap through the generic check, which throws the
    // branded exception for any non-zero code (runtime codes included).
    assert!(
        dart.contains("throw WeaveFFIException(code, msg);"),
        "generic check must throw the branded exception: {dart}"
    );
    // A non-throwing async completion also builds the generic exception
    // rather than the domain mapper.
    let count_body = &dart[dart.find("int count()").expect("count body")..];
    assert!(
        count_body.contains("_checkError(err);"),
        "non-throwing member must trap generically: {dart}"
    );
}
