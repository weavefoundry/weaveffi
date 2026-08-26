use super::*;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
    StructField,
};

fn make_api(modules: Vec<Module>) -> Api {
    Api {
        version: "0.6.0".to_string(),
        modules,
        generators: None,
        package: None,
    }
}

/// Build the binding model and render the wrapper, exactly as the driver
/// does in production before calling [`LanguageBackend::files`].
fn render(
    api: &Api,
    c_prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let model = BindingModel::build(api, c_prefix);
    render_swift_wrapper(
        api,
        &model,
        c_prefix,
        strip_module_prefix,
        input_basename,
        filename,
    )
}

fn empty_module(name: &str) -> Module {
    Module {
        name: name.into(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }
}

#[test]
fn package_uses_binary_target_and_bundles_slices() {
    use camino::Utf8Path;
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = make_api(vec![empty_module("calc")]);
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::MacosX64, "/s/darwin-x64/libcalculator.dylib");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &SwiftGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &SwiftConfig::default(),
    )
    .expect("swift supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("swift/lib/darwin-arm64/libcalculator.dylib")));
    let pkg = files
        .iter()
        .find(|f| f.path.as_str().ends_with("swift/Package.swift"))
        .expect("Package.swift present");
    let FileContent::Text(txt) = &pkg.content else {
        panic!("Package.swift is text");
    };
    assert!(
        txt.contains(".binaryTarget(") && txt.contains(".xcframework"),
        "binaryTarget xcframework missing: {txt}"
    );
}

#[test]
fn listeners_generate_register_unregister() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnMessage".into(),
            doc: None,
            params: vec![Param {
                name: "message".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let swift = render(&api, "weaveffi", false, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        swift.contains("final class WvCallbackBox<T>"),
        "callback box must be emitted: {swift}"
    );
    assert!(
        swift.contains(
            "public static func eventsRegisterMessageListener(_ callback: @escaping (String) -> Void) -> UInt64"
        ),
        "register wrapper missing: {swift}"
    );
    assert!(
        swift.contains("public static func eventsUnregisterMessageListener(_ id: UInt64)"),
        "unregister wrapper missing: {swift}"
    );
    assert!(
        swift.contains("cb(String(cString: message!))"),
        "trampoline must convert the string arg: {swift}"
    );
    assert!(
        swift.contains("Unmanaged.passRetained(box).toOpaque()"),
        "closure box must be retained through context: {swift}"
    );
    assert!(
        swift.contains(".fromOpaque(ctx).release()"),
        "unregister must release the retained box: {swift}"
    );
}

#[test]
fn listener_decodes_buffered_argument() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnContact".into(),
            doc: None,
            params: vec![Param {
                name: "contact".into(),
                ty: TypeRef::Record("Contact".into()),
                mutable: false,
                doc: None,
            }],
        }],
        listeners: vec![ListenerDef {
            name: "contact_listener".into(),
            event_callback: "OnContact".into(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let swift = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // The user closure receives the decoded value type, not raw pointers.
    assert!(
        swift.contains("_ callback: @escaping (Contact) -> Void"),
        "listener closure must take the decoded record: {swift}"
    );
    // The borrowed (ptr, len) pair is copied and decoded inside the
    // trampoline, before the user closure is invoked, and never freed.
    assert!(
        swift.contains(
            "let contactBuf = [UInt8](UnsafeBufferPointer(start: contact_ptr, count: contact_len))"
        ),
        "trampoline must copy the borrowed buffer: {swift}"
    );
    assert!(
        swift.contains("wvReadContact(&contactReader)"),
        "trampoline must decode the record: {swift}"
    );
    assert!(
        swift.contains("cb(v0)"),
        "trampoline must pass the decoded value: {swift}"
    );
}

#[test]
fn swift_type_for_struct_returns_name() {
    assert_eq!(
        swift_type_for(&TypeRef::Record("Contact".into())),
        "Contact"
    );
}

#[test]
fn swift_type_for_enum_returns_name() {
    assert_eq!(swift_type_for(&TypeRef::Enum("Color".into())), "Color");
}

#[test]
fn swift_type_for_optional_wraps_inner() {
    assert_eq!(
        swift_type_for(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "Int32?"
    );
    assert_eq!(
        swift_type_for(&TypeRef::Optional(Box::new(TypeRef::Record(
            "Contact".into()
        )))),
        "Contact?"
    );
}

#[test]
fn swift_type_for_list_wraps_inner() {
    assert_eq!(
        swift_type_for(&TypeRef::List(Box::new(TypeRef::I32))),
        "[Int32]"
    );
    assert_eq!(
        swift_type_for(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
        "[Color]"
    );
}

fn plain_fn(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
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

fn p(name: &str, ty: TypeRef) -> Param {
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

fn variant(name: &str, value: i32) -> EnumVariant {
    EnumVariant {
        name: name.into(),
        value,
        doc: None,
        fields: vec![],
    }
}

#[test]
fn render_enum_declaration() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![variant("Red", 0), variant("Green", 1), variant("Blue", 2)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public enum Color: UInt32 {"),
        "missing enum declaration: {out}"
    );
    assert!(out.contains("case red = 0"), "missing red variant: {out}");
    assert!(
        out.contains("case green = 1"),
        "missing green variant: {out}"
    );
    assert!(out.contains("case blue = 2"), "missing blue variant: {out}");
}

#[test]
fn render_enum_variant_camel_case() {
    let api = make_api(vec![Module {
        name: "status".to_string(),
        functions: vec![],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Status".to_string(),
            doc: None,
            variants: vec![variant("InProgress", 0), variant("AllDone", 1)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("case inProgress = 0"),
        "missing camelCase variant: {out}"
    );
    assert!(
        out.contains("case allDone = 1"),
        "missing camelCase variant: {out}"
    );
}

#[test]
fn render_function_with_enum_param_and_return() {
    let api = make_api(vec![Module {
        name: "paint".to_string(),
        functions: vec![plain_fn(
            "mix",
            vec![p("a", TypeRef::Enum("Color".into()))],
            Some(TypeRef::Enum("Color".into())),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(out.contains("a: Color"), "missing enum param type: {out}");
    assert!(
        out.contains("-> Color {"),
        "missing enum return type: {out}"
    );
    assert!(
        out.contains("weaveffi_paint_Color(a.rawValue)"),
        "missing enum-to-C conversion: {out}"
    );
    assert!(
        out.contains("Color(rawValue: rv.rawValue)!"),
        "missing C-to-enum conversion: {out}"
    );
}

#[test]
fn render_function_with_optional_value_param() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "find",
            vec![p("id", TypeRef::Optional(Box::new(TypeRef::I32)))],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("id: Int32?"),
        "missing optional param type: {out}"
    );
    // The optional is packed into a value buffer: flag byte plus payload.
    assert!(
        out.contains("var idWriter = WvWriter()"),
        "missing buffer writer staging: {out}"
    );
    assert!(
        out.contains("idWriter.writeOptionFlag(true)")
            && out.contains("idWriter.writeOptionFlag(false)"),
        "missing option flag encoding: {out}"
    );
    assert!(
        out.contains("idWriter.writeInt32(v0)"),
        "missing payload encoding: {out}"
    );
    assert!(
        out.contains("idWriter.bytes.withUnsafeBufferPointer { id_buf in"),
        "missing buffer staging closure: {out}"
    );
    assert!(
        out.contains("weaveffi_store_find(id_ptr, id_len, &err)"),
        "missing two-slot buffered call: {out}"
    );
}

#[test]
fn render_function_with_optional_struct_param() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "update",
            vec![p(
                "person",
                TypeRef::Optional(Box::new(TypeRef::Record("Contact".into()))),
            )],
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

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("person: Contact?"),
        "missing optional struct param: {out}"
    );
    assert!(
        out.contains("wvWriteContact(v0, into: &personWriter)"),
        "optional record must pack through the record codec: {out}"
    );
    assert!(
        out.contains("weaveffi_contacts_update(person_ptr, person_len, &err)"),
        "missing buffered call: {out}"
    );
}

#[test]
fn render_function_with_optional_value_return() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "lookup",
            vec![p("key", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::I32))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> Int32? {"),
        "missing optional return type: {out}"
    );
    assert!(
        out.contains("var outLen: Int = 0"),
        "missing outLen declaration: {out}"
    );
    assert!(
        out.contains("rvReader.readOptionFlag()"),
        "missing option flag decode: {out}"
    );
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)"),
        "returned buffer must be freed after copying: {out}"
    );
}

#[test]
fn render_function_with_optional_string_return() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "get_name",
            vec![],
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

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> String? {"),
        "missing optional string return type: {out}"
    );
    // An optional string is buffered: flag byte plus length-prefixed UTF-8.
    assert!(
        out.contains("rvReader.readOptionFlag()") && out.contains("rvReader.readString()"),
        "missing buffered optional-string decode: {out}"
    );
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)"),
        "returned buffer must be freed after decoding: {out}"
    );
}

#[test]
fn render_function_with_list_param() {
    let api = make_api(vec![Module {
        name: "batch".to_string(),
        functions: vec![plain_fn(
            "process",
            vec![p("ids", TypeRef::List(Box::new(TypeRef::I32)))],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("ids: [Int32]"),
        "missing list param type: {out}"
    );
    assert!(
        out.contains("idsWriter.writeLen(ids.count)"),
        "missing count prefix: {out}"
    );
    assert!(
        out.contains("for v0 in ids {") && out.contains("idsWriter.writeInt32(v0)"),
        "missing element encoding loop: {out}"
    );
    assert!(
        out.contains("idsWriter.bytes.withUnsafeBufferPointer { ids_buf in"),
        "missing withUnsafeBufferPointer: {out}"
    );
    assert!(
        out.contains("weaveffi_batch_process(ids_ptr, ids_len, &err)"),
        "missing buffered call: {out}"
    );
}

#[test]
fn render_function_with_list_return() {
    let api = make_api(vec![Module {
        name: "batch".to_string(),
        functions: vec![plain_fn(
            "get_ids",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::I32))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> [Int32] {"),
        "missing list return type: {out}"
    );
    assert!(
        out.contains("var outLen: Int = 0"),
        "missing outLen declaration: {out}"
    );
    assert!(out.contains("&outLen"), "missing outLen in call: {out}");
    assert!(
        out.contains("rvReader.readLen()"),
        "missing count decode: {out}"
    );
    assert!(
        out.contains("v0.append(v2)") && out.contains("return v0"),
        "missing element decode loop: {out}"
    );
}

#[test]
fn render_function_with_optional_struct_return() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "find",
            vec![p("id", TypeRef::I32)],
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

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> Contact? {"),
        "missing optional struct return: {out}"
    );
    assert!(
        out.contains("wvReadContact(&rvReader)"),
        "optional record must decode through the record codec: {out}"
    );
}

#[test]
fn render_buffer_runtime_is_always_emitted() {
    let api = make_api(vec![]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("struct WvWriter"),
        "missing buffer writer: {out}"
    );
    assert!(
        out.contains("struct WvReader"),
        "missing buffer reader: {out}"
    );
    assert!(
        out.contains("func wvDecodeFailure"),
        "missing decode-failure trap: {out}"
    );
    // Little-endian assembly, one byte at a time (no alignment assumptions).
    assert!(
        out.contains("v |= UInt32(b) << (8 * i)"),
        "reader must assemble little-endian: {out}"
    );
    assert!(
        out.contains("bytes.append(UInt8(truncatingIfNeeded: v >> 8))"),
        "writer must emit little-endian: {out}"
    );
    // Malformed input rejection: truncation, invalid flags, oversized
    // length prefixes, and trailing bytes.
    assert!(
        out.contains("length prefix exceeds remaining buffer"),
        "missing length-prefix validation: {out}"
    );
    assert!(
        out.contains("trailing bytes after value"),
        "missing trailing-bytes validation: {out}"
    );
    assert!(
        out.contains("option flag byte out of range"),
        "missing flag validation: {out}"
    );
}

#[test]
fn render_record_as_swift_struct() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public struct Contact {"),
        "record must be a plain struct: {out}"
    );
    assert!(
        out.contains("public var name: String") && out.contains("public var age: Int32"),
        "missing stored properties: {out}"
    );
    assert!(
        out.contains("public init(name: String, age: Int32) {"),
        "missing public memberwise init: {out}"
    );
    // One pack and one unpack routine, fields in declaration order.
    assert!(
        out.contains("func wvWriteContact(_ value: Contact, into w: inout WvWriter) {"),
        "missing pack routine: {out}"
    );
    assert!(
        out.contains("w.writeString(value.name)") && out.contains("w.writeInt32(value.age)"),
        "pack must write fields in order: {out}"
    );
    assert!(
        out.contains("func wvReadContact(_ r: inout WvReader) -> Contact {"),
        "missing unpack routine: {out}"
    );
    assert!(
        out.contains("return Contact(name: v0, age: v1)"),
        "unpack must rebuild the struct: {out}"
    );
    // No class wrapping, no C symbols, no getters, no builders.
    assert!(
        !out.contains("public class Contact"),
        "record must not be a class: {out}"
    );
    assert!(
        !out.contains("Contact_destroy") && !out.contains("deinit"),
        "record has no destroy: {out}"
    );
    assert!(
        !out.contains("Contact_get_name"),
        "record has no FFI getters: {out}"
    );
    assert!(
        !out.contains("ContactBuilder"),
        "record has no builder: {out}"
    );
}

#[test]
fn swift_custom_prefix_threads_to_user_symbols() {
    let api = make_api(vec![Module {
        name: "demo".to_string(),
        functions: vec![plain_fn(
            "paint",
            vec![p("c", TypeRef::Enum("Color".into()))],
            Some(TypeRef::Enum("Color".into())),
        )],
        structs: vec![StructDef {
            name: "Point".to_string(),
            doc: None,
            fields: vec![field("x", TypeRef::I32), field("y", TypeRef::I32)],
        }],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![variant("Red", 0), variant("Green", 1)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_swift_custom_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");
    let config = SwiftConfig {
        prefix: Some("myffi".to_string()),
        ..Default::default()
    };
    SwiftGenerator.generate(&api, out_dir, &config).unwrap();

    let swift = std::fs::read_to_string(
        tmp.join("swift")
            .join("Sources")
            .join("WeaveFFI")
            .join("WeaveFFI.swift"),
    )
    .unwrap();
    let modulemap = std::fs::read_to_string(
        tmp.join("swift")
            .join("Sources")
            .join("CWeaveFFI")
            .join("module.modulemap"),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    // User symbols honor the configured ABI prefix: the function symbol and
    // the enum-to-C cast both carry `myffi_`.
    assert!(
        swift.contains("myffi_demo_paint"),
        "function user symbol should use custom prefix: {swift}"
    );
    assert!(
        swift.contains("myffi_demo_Color("),
        "enum-cast user symbol should use custom prefix: {swift}"
    );
    // No user symbol falls back to the hard-coded `weaveffi_` prefix.
    assert!(
        !swift.contains("weaveffi_demo_"),
        "no user symbol should keep the default prefix: {swift}"
    );
    // The system module map points at the prefixed C header.
    assert!(
        modulemap.contains("header \"../../../c/myffi.h\""),
        "module map should reference the prefixed C header: {modulemap}"
    );
    // Runtime ABI helpers stay literal regardless of the prefix.
    assert!(
        swift.contains("weaveffi_error_clear(&err)"),
        "runtime helper must remain literal: {swift}"
    );
}

#[test]
fn render_function_returning_struct() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "create",
            vec![p("age", TypeRef::I32)],
            Some(TypeRef::Record("Contact".into())),
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("age", TypeRef::I32)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> Contact {"),
        "missing struct return type: {out}"
    );
    // A record return is a buffered return: ptr plus trailing out_len.
    assert!(
        out.contains("let rv = weaveffi_contacts_create(age, &outLen, &err)"),
        "missing buffered call: {out}"
    );
    assert!(
        out.contains("wvReadContact(&rvReader)"),
        "missing record decode: {out}"
    );
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)"),
        "returned buffer must be freed after copying: {out}"
    );
    assert!(
        out.contains("rvReader.finish()"),
        "decode must reject trailing bytes: {out}"
    );
}

#[test]
fn render_function_with_struct_param() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "save",
            vec![p("contact", TypeRef::Record("Contact".into()))],
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

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("contact: Contact"),
        "missing struct param type: {out}"
    );
    // The record is packed into a caller-owned buffer and passed as two
    // slots; the callee never frees it.
    assert!(
        out.contains("var contactWriter = WvWriter()")
            && out.contains("wvWriteContact(contact, into: &contactWriter)"),
        "missing record packing: {out}"
    );
    assert!(
        out.contains("weaveffi_contacts_save(contact_ptr, contact_len, &err)"),
        "missing two-slot buffered call: {out}"
    );
    assert!(
        !out.contains("contact.ptr"),
        "record param must not marshal as an object pointer: {out}"
    );
}

#[test]
fn render_struct_with_bytes_field() {
    let api = make_api(vec![Module {
        name: "storage".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Blob".to_string(),
            doc: None,
            fields: vec![field("data", TypeRef::Bytes)],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public var data: Data"),
        "missing bytes property: {out}"
    );
    assert!(
        out.contains("w.writeBytes(value.data)"),
        "pack must length-prefix the bytes: {out}"
    );
    assert!(
        out.contains("let v0 = r.readBytes()"),
        "unpack must decode the bytes: {out}"
    );
}

#[test]
fn render_struct_with_nested_struct_field() {
    let api = make_api(vec![Module {
        name: "geo".to_string(),
        functions: vec![],
        structs: vec![
            StructDef {
                name: "Point".to_string(),
                doc: None,
                fields: vec![field("x", TypeRef::I32)],
            },
            StructDef {
                name: "Line".to_string(),
                doc: None,
                fields: vec![field("start", TypeRef::Record("Point".into()))],
            },
        ],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public var start: Point"),
        "missing nested struct property: {out}"
    );
    // Nested records delegate to the inner codec.
    assert!(
        out.contains("wvWritePoint(value.start, into: &w)"),
        "pack must delegate to the nested codec: {out}"
    );
    assert!(
        out.contains("let v0 = wvReadPoint(&r)"),
        "unpack must delegate to the nested codec: {out}"
    );
}

#[test]
fn render_function_returning_struct_with_buffer_params() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "find_by_name",
            vec![p("query", TypeRef::StringUtf8)],
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

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> Contact {"),
        "missing struct return type with buffer params: {out}"
    );
    // The staged closures return the raw buffer pointer; the decode happens
    // after the closures complete.
    assert!(
        out.contains("let rv: UnsafePointer<UInt8>? = query.withCString { query_ptr in"),
        "missing annotated closure binding: {out}"
    );
    assert!(
        out.contains("return weaveffi_contacts_find_by_name(query_ptr, &outLen, &err)"),
        "missing buffered call inside the closure: {out}"
    );
    assert!(
        out.contains("wvReadContact(&rvReader)"),
        "missing record decode after the call: {out}"
    );
}

#[test]
fn generate_swift_with_structs_and_enums() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "get_contact",
            vec![p("id", TypeRef::I32)],
            Some(TypeRef::Record("Contact".into())),
        )],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("email", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
            ],
        }],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![variant("Red", 0), variant("Green", 1), variant("Blue", 2)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_swift_structs_and_enums");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    SwiftGenerator
        .generate(
            &api,
            out_dir,
            &SwiftConfig {
                strip_module_prefix: true,
                ..SwiftConfig::default()
            },
        )
        .unwrap();

    let swift = std::fs::read_to_string(
        tmp.join("swift")
            .join("Sources")
            .join("WeaveFFI")
            .join("WeaveFFI.swift"),
    )
    .unwrap();

    assert!(
        swift.contains("public enum Color: UInt32 {"),
        "missing enum declaration: {swift}"
    );
    assert!(swift.contains("case red = 0"), "missing red case: {swift}");
    assert!(
        swift.contains("case green = 1"),
        "missing green case: {swift}"
    );
    assert!(
        swift.contains("case blue = 2"),
        "missing blue case: {swift}"
    );

    assert!(
        swift.contains("public struct Contact {"),
        "missing struct declaration: {swift}"
    );
    assert!(
        swift.contains("public var name: String")
            && swift.contains("public var email: String")
            && swift.contains("public var age: Int32"),
        "missing stored properties: {swift}"
    );

    assert!(
        swift.contains("public static func getContact(id: Int32) -> Contact {"),
        "missing function signature: {swift}"
    );
    assert!(
        swift.contains("wvReadContact(&rvReader)"),
        "missing record decode: {swift}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn swift_type_for_map() {
    assert_eq!(
        swift_type_for(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "[String: Int32]"
    );
    assert_eq!(
        swift_type_for(&TypeRef::Map(
            Box::new(TypeRef::I32),
            Box::new(TypeRef::F64)
        )),
        "[Int32: Double]"
    );
}

#[test]
fn render_function_with_map_param() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "update_scores",
            vec![p(
                "scores",
                TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64)),
            )],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("scores: [Int32: Double]"),
        "missing map param type: {out}"
    );
    // The map packs into one buffer: count then alternating key, value.
    assert!(
        out.contains("scoresWriter.writeLen(scores.count)"),
        "missing count prefix: {out}"
    );
    assert!(
        out.contains("for (v0, v1) in scores {"),
        "missing entry loop: {out}"
    );
    assert!(
        out.contains("scoresWriter.writeInt32(v0)") && out.contains("scoresWriter.writeDouble(v1)"),
        "missing key/value encoding: {out}"
    );
    assert!(
        out.contains("weaveffi_store_update_scores(scores_ptr, scores_len, &err)"),
        "missing single-buffer call: {out}"
    );
}

#[test]
fn render_function_with_map_return() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "get_scores",
            vec![],
            Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("-> [Int32: Double] {"),
        "missing map return type: {out}"
    );
    assert!(out.contains("var outLen: Int = 0"), "missing outLen: {out}");
    assert!(
        out.contains("var v0: [Int32: Double] = [:]"),
        "missing dict construction: {out}"
    );
    assert!(
        out.contains("v0[v2] = v3"),
        "missing entry decode loop: {out}"
    );
    // No parallel key/value arrays remain.
    assert!(
        !out.contains("outKeysPtr") && !out.contains("outValuesPtr"),
        "map return must not use parallel arrays: {out}"
    );
}

#[test]
fn swift_struct_optional_fields_are_properties() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                field("age", TypeRef::Optional(Box::new(TypeRef::I32))),
                field(
                    "role",
                    TypeRef::Optional(Box::new(TypeRef::Enum("Role".into()))),
                ),
            ],
        }],
        enums: vec![EnumDef {
            name: "Role".into(),
            doc: None,
            variants: vec![variant("Admin", 0), variant("User", 1)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    assert!(
        out.contains("public var email: String?"),
        "missing optional string property: {out}"
    );
    assert!(
        out.contains("public var age: Int32?"),
        "missing optional i32 property: {out}"
    );
    assert!(
        out.contains("public var role: Role?"),
        "missing optional enum property: {out}"
    );
    // The codec writes the option flag per field.
    assert!(
        out.contains("w.writeOptionFlag(true)") && out.contains("w.writeOptionFlag(false)"),
        "codec must encode option flags: {out}"
    );
    assert!(
        out.contains("Role(rawValue: UInt32(bitPattern: r.readInt32()))!"),
        "codec must decode the enum from its i32 wire form: {out}"
    );
}

#[test]
fn swift_custom_module_name() {
    let api = make_api(vec![Module {
        name: "math".to_string(),
        functions: vec![plain_fn(
            "add",
            vec![p("a", TypeRef::I32), p("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let config = SwiftConfig {
        module_name: Some("MyCoolLib".into()),
        ..SwiftConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_swift_custom_module");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    SwiftGenerator.generate(&api, out_dir, &config).unwrap();

    let pkg = std::fs::read_to_string(tmp.join("swift").join("Package.swift")).unwrap();
    assert!(
        pkg.contains("name: \"MyCoolLib\""),
        "Package.swift should use custom module name: {pkg}"
    );
    assert!(
        pkg.contains("\"CMyCoolLib\""),
        "Package.swift should reference CMyCoolLib: {pkg}"
    );
    assert!(
        !pkg.contains("\"WeaveFFI\""),
        "Package.swift should not reference WeaveFFI as a module name: {pkg}"
    );

    let modulemap = std::fs::read_to_string(
        tmp.join("swift")
            .join("Sources")
            .join("CMyCoolLib")
            .join("module.modulemap"),
    )
    .unwrap();
    assert!(
        modulemap.contains("module CMyCoolLib"),
        "modulemap should use custom name: {modulemap}"
    );

    let swift_src = tmp
        .join("swift")
        .join("Sources")
        .join("MyCoolLib")
        .join("MyCoolLib.swift");
    assert!(
        swift_src.exists(),
        "Swift source should be at MyCoolLib/MyCoolLib.swift"
    );

    let swift = std::fs::read_to_string(&swift_src).unwrap();
    assert!(
        swift.contains("import CMyCoolLib"),
        "wrapper must import the renamed C module: {swift}"
    );
    assert!(
        !swift.contains("import CWeaveFFI"),
        "wrapper must not import the default C module when renamed: {swift}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn swift_inline_error_types() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "get",
            vec![p("id", TypeRef::I32)],
            Some(TypeRef::I32),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "ContactError".to_string(),
            codes: vec![
                ErrorCode {
                    name: "ContactNotFound".to_string(),
                    code: 1001,
                    message: "Contact not found".to_string(),
                    doc: None,
                    fields: vec![],
                },
                ErrorCode {
                    name: "InvalidInput".to_string(),
                    code: 1002,
                    message: "Invalid input provided".to_string(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    // The generic brand error keeps only the unknown-code case; the
    // domain gets its own typed enum.
    assert!(
        out.contains("public enum WeaveFFIError: Error, LocalizedError {"),
        "missing brand error: {out}"
    );
    assert!(
        out.contains("public enum ContactError: Error, LocalizedError {"),
        "missing typed error enum: {out}"
    );
    assert!(
        out.contains("case contactNotFound(message: String)"),
        "missing contactNotFound case: {out}"
    );
    assert!(
        out.contains("case invalidInput(message: String)"),
        "missing invalidInput case: {out}"
    );
    assert!(
        out.contains("public var errorDescription: String?"),
        "missing errorDescription property: {out}"
    );
    assert!(
        out.contains("public var errorCode: Int32"),
        "missing errorCode property: {out}"
    );
    assert!(
        out.contains("case .contactNotFound: return 1001"),
        "missing contactNotFound code: {out}"
    );
    assert!(
        out.contains("case .invalidInput: return 1002"),
        "missing invalidInput code: {out}"
    );
    assert!(
        out.contains(
            "func mapContacts(code: Int32, message: String, payload: [UInt8]?) -> Error {"
        ),
        "missing domain mapper: {out}"
    );
    assert!(
        out.contains(
            "case 1001: return ContactError.contactNotFound(message: message.isEmpty ? \"Contact not found\" : message)"
        ),
        "missing contactNotFound mapping: {out}"
    );
    assert!(
        out.contains(
            "case 1002: return ContactError.invalidInput(message: message.isEmpty ? \"Invalid input provided\" : message)"
        ),
        "missing invalidInput mapping: {out}"
    );
    assert!(
        out.contains("default: return WeaveFFIError.error(code: code, message: message)"),
        "missing unknown-code fallback: {out}"
    );
    assert!(
        out.contains("func checkContacts(_ err: inout weaveffi_error) throws {"),
        "missing domain checker: {out}"
    );
    assert!(
        out.contains("throw mapContacts(code: code, message: message, payload: payload)"),
        "domain checker must throw through the mapper: {out}"
    );
}

#[test]
fn swift_error_payload_fields_decode() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![Function {
            name: "put".into(),
            params: vec![p("key", TypeRef::StringUtf8)],
            returns: None,
            doc: None,
            throws: true,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".to_string(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".to_string(),
                    code: 1001,
                    message: "Key not found".to_string(),
                    doc: None,
                    fields: vec![],
                },
                ErrorCode {
                    name: "QuotaExceeded".to_string(),
                    code: 1002,
                    message: "Quota exceeded".to_string(),
                    doc: None,
                    fields: vec![field("limit", TypeRef::U32), field("used", TypeRef::U32)],
                },
            ],
        }),
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    // A code with declared fields carries them as labeled associated values
    // after the message.
    assert!(
        out.contains("case quotaExceeded(message: String, limit: UInt32, used: UInt32)"),
        "missing payload associated values: {out}"
    );
    assert!(
        out.contains("case keyNotFound(message: String)"),
        "field-less code keeps the message-only shape: {out}"
    );
    // errorDescription wildcards the payload fields.
    assert!(
        out.contains("case let .quotaExceeded(message, _, _): return message"),
        "errorDescription must wildcard payload fields: {out}"
    );
    // The mapper decodes the payload buffer in declaration order.
    assert!(
        out.contains("var payloadReader = WvReader(bytes: payload ?? [])"),
        "mapper must read the payload buffer: {out}"
    );
    assert!(
        out.contains("payloadReader.readUInt32()"),
        "mapper must decode the fields: {out}"
    );
    assert!(
        out.contains("payloadReader.finish()"),
        "mapper must reject trailing payload bytes: {out}"
    );
    assert!(
        out.contains("limit: v0, used: v1"),
        "mapper must label the decoded fields: {out}"
    );
    // The checker copies the payload before clearing the error slot.
    assert!(
        out.contains(
            "let payload: [UInt8]? = err.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.payload_len)) }"
        ),
        "checker must copy the payload before clearing: {out}"
    );
    // The error slot initializer covers the payload fields.
    assert!(
        out.contains("weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)"),
        "error slot init must cover the payload fields: {out}"
    );
}

#[test]
fn swift_struct_list_field_is_property() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Order".to_string(),
            doc: None,
            fields: vec![
                field("item_ids", TypeRef::List(Box::new(TypeRef::I32))),
                field("tags", TypeRef::List(Box::new(TypeRef::Enum("Tag".into())))),
            ],
        }],
        enums: vec![EnumDef {
            name: "Tag".into(),
            doc: None,
            variants: vec![variant("New", 0), variant("Sale", 1)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    // Field names camel-case into Swift property names.
    assert!(
        out.contains("public var itemIds: [Int32]"),
        "missing list i32 property: {out}"
    );
    assert!(
        out.contains("public var tags: [Tag]"),
        "missing list enum property: {out}"
    );
    assert!(
        out.contains("w.writeLen(value.itemIds.count)"),
        "codec must count-prefix the list: {out}"
    );
    assert!(
        out.contains("Tag(rawValue: UInt32(bitPattern: r.readInt32()))!"),
        "codec must decode the enum elements: {out}"
    );
}

#[test]
fn swift_strip_module_prefix() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![plain_fn(
            "create_contact",
            vec![p("name", TypeRef::StringUtf8)],
            Some(TypeRef::I32),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let config = SwiftConfig {
        strip_module_prefix: true,
        ..SwiftConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_swift_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    SwiftGenerator.generate(&api, out_dir, &config).unwrap();

    let swift = std::fs::read_to_string(tmp.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

    assert!(
        swift.contains("func createContact("),
        "stripped name should be createContact: {swift}"
    );
    assert!(
        !swift.contains("func contactsCreateContact("),
        "should not contain module-prefixed name: {swift}"
    );
    assert!(
        swift.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {swift}"
    );

    let no_strip_config = SwiftConfig {
        strip_module_prefix: false,
        ..SwiftConfig::default()
    };
    let tmp2 = std::env::temp_dir().join("weaveffi_test_swift_no_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp2);
    std::fs::create_dir_all(&tmp2).unwrap();
    let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

    SwiftGenerator
        .generate(&api, out_dir2, &no_strip_config)
        .unwrap();

    let swift2 =
        std::fs::read_to_string(tmp2.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

    assert!(
        swift2.contains("func contactsCreateContact("),
        "strip_module_prefix: false should restore the prefixed name: {swift2}"
    );
    assert!(
        swift2.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {swift2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&tmp2);
}

#[test]
fn swift_deeply_nested_optional() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![plain_fn(
            "process",
            vec![p(
                "data",
                TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                    Box::new(TypeRef::Record("Contact".into())),
                ))))),
            )],
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
    let swift = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        swift.contains("[Contact?]?"),
        "should contain deeply nested optional type: {swift}"
    );
}

#[test]
fn swift_map_of_lists() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![plain_fn(
            "process",
            vec![p(
                "scores",
                TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                ),
            )],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let swift = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        swift.contains("[String: [Int32]]"),
        "should contain map of lists type: {swift}"
    );
}

#[test]
fn swift_enum_keyed_map() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![plain_fn(
            "process",
            vec![p(
                "contacts",
                TypeRef::Map(
                    Box::new(TypeRef::Enum("Color".into())),
                    Box::new(TypeRef::Record("Contact".into())),
                ),
            )],
            None,
        )],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![variant("Red", 0), variant("Green", 1), variant("Blue", 2)],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let swift = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        swift.contains("[Color: Contact]"),
        "should contain enum-keyed map type: {swift}"
    );
}

#[test]
fn swift_type_for_borrowed_str() {
    assert_eq!(swift_type_for(&TypeRef::BorrowedStr), "String");
}

#[test]
fn swift_type_for_borrowed_bytes() {
    assert_eq!(swift_type_for(&TypeRef::BorrowedBytes), "Data");
}

#[test]
fn swift_function_with_borrowed_str_param() {
    let api = make_api(vec![Module {
        name: "io".to_string(),
        functions: vec![plain_fn(
            "write",
            vec![p("msg", TypeRef::BorrowedStr)],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("msg: String"),
        "BorrowedStr param should use String type: {out}"
    );
    assert!(
        out.contains("weaveffi_io_write"),
        "should call the C function: {out}"
    );
}

#[test]
fn swift_function_with_borrowed_bytes_param() {
    let api = make_api(vec![Module {
        name: "io".to_string(),
        functions: vec![plain_fn(
            "upload",
            vec![p("data", TypeRef::BorrowedBytes)],
            None,
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("data: Data"),
        "BorrowedBytes param should use Data type: {out}"
    );
    assert!(
        out.contains("weaveffi_io_upload"),
        "should call the C function: {out}"
    );
}

#[test]
fn swift_typed_handle_is_uint64() {
    let api = make_api(vec![Module {
        name: "auth".into(),
        functions: vec![
            plain_fn(
                "revoke",
                vec![p("session", TypeRef::TypedHandle("Session".into()))],
                None,
            ),
            plain_fn("open", vec![], Some(TypeRef::TypedHandle("Session".into()))),
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let swift = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // A typed handle is a `UInt64` token in Swift, reinterpreted as the
    // opaque typed pointer at the C boundary.
    assert!(
        swift.contains("session: UInt64"),
        "typed handle param must be UInt64: {swift}"
    );
    assert!(
        swift.contains("OpaquePointer(bitPattern: UInt(session))"),
        "typed handle param must convert to the C pointer: {swift}"
    );
    assert!(
        swift.contains("-> UInt64 {"),
        "typed handle return must be UInt64: {swift}"
    );
    assert!(
        swift.contains("return UInt64(UInt(bitPattern: rv))"),
        "typed handle return must convert from the C pointer: {swift}"
    );
}

#[test]
fn swift_no_double_free_on_error() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }],
        enums: vec![],
        callbacks: vec![],
        functions: vec![plain_fn(
            "find_contact",
            vec![p("name", TypeRef::StringUtf8)],
            Some(TypeRef::Record("Contact".into())),
        )],
        interfaces: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    assert!(
        !out.contains("weaveffi_free_string(name"),
        "borrowed string param must not be freed by the wrapper: {out}"
    );

    // The error must be checked before the returned buffer is touched.
    let fn_start = out
        .find("public static func findContact")
        .expect("findContact wrapper");
    let fn_body = &out[fn_start..];
    let check_pos = fn_body
        .find("trap(&err)")
        .expect("trap in findContact (non-throwing wrapper)");
    let decode_pos = fn_body
        .find("wvReadContact(")
        .expect("record decode in findContact");
    let free_pos = fn_body
        .find("weaveffi_free_bytes(")
        .expect("buffer free in findContact");
    assert!(
        check_pos < decode_pos && check_pos < free_pos,
        "error must be checked before decoding or freeing the return: {out}"
    );
}

#[test]
fn swift_optional_interface_return_maps_null() {
    use weaveffi_ir::ir::InterfaceDef;
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![plain_fn(
            "find_store",
            vec![p("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                "Store".into(),
            )))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![],
            methods: vec![],
            statics: vec![],
        }],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // Optional<Interface> stays a nullable pointer: null means none.
    assert!(
        out.contains("-> Store? {"),
        "missing optional interface return type: {out}"
    );
    assert!(
        out.contains("return rv.map { Store(ptr: $0) }"),
        "optional interface return should map null before wrapping: {out}"
    );
}

#[test]
fn swift_async_function_signature() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "run".to_string(),
            params: vec![p("id", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: true,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: Some(ErrorDomain {
            name: "TaskError".to_string(),
            codes: vec![ErrorCode {
                name: "Busy".to_string(),
                code: 1,
                message: "Busy".to_string(),
                doc: None,
                fields: vec![],
            }],
        }),
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public static func run(id: Int32) async throws -> Int32"),
        "missing complete async throws signature: {out}"
    );
    assert!(
        out.contains("withCheckedThrowingContinuation"),
        "throwing async must use the throwing continuation: {out}"
    );
    assert!(
        out.contains("resume(throwing: mapTasks(code: code, message: msg, payload: payload))"),
        "callback must resume with the typed domain error: {out}"
    );
}

#[test]
fn swift_async_uses_continuation() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "run".to_string(),
            params: vec![p("id", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // A non-throwing async function uses the plain continuation flavor
    // and traps if the slot ever reports (producer panic).
    assert!(
        out.contains("public static func run(id: Int32) async -> Int32"),
        "missing plain async signature: {out}"
    );
    assert!(
        out.contains("withCheckedContinuation"),
        "missing withCheckedContinuation: {out}"
    );
    assert!(
        out.contains("CheckedContinuation<Int32, Never>"),
        "plain async must use the Never-typed continuation: {out}"
    );
    assert!(
        out.contains("ContinuationRef"),
        "missing ContinuationRef usage: {out}"
    );
    assert!(
        out.contains("Unmanaged"),
        "missing Unmanaged for context bridging: {out}"
    );
    assert!(
        out.contains("weaveffi_tasks_run_async"),
        "missing async C function call: {out}"
    );
}

/// `Unmanaged.passRetained(...)` (the +1 retain that pins the
/// continuation across the C boundary) must be matched by exactly one
/// `Unmanaged.fromOpaque(...).takeRetainedValue()` in the C callback so
/// the continuation is released when the future resolves.
#[test]
fn swift_async_pins_callback_for_lifetime() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "run".to_string(),
            params: vec![p("id", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    let pin_count = out.matches("Unmanaged.passRetained").count();
    let unpin_count = out.matches("takeRetainedValue()").count();
    assert_eq!(
        pin_count, 1,
        "expected exactly one Unmanaged.passRetained, found {pin_count}: {out}"
    );
    assert_eq!(
        unpin_count, 1,
        "expected exactly one takeRetainedValue, found {unpin_count}: {out}"
    );
}

#[test]
fn swift_async_buffered_result_decoded_in_callback() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "fetch_ids".to_string(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
            doc: None,
            throws: true,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // The callback receives a borrowed (ptr, len) pair.
    assert!(
        out.contains("{ context, err, resultPtr, resultLen in"),
        "callback must take the borrowed buffer pair: {out}"
    );
    // The buffer is copied and decoded inside the callback, before resuming.
    assert!(
        out.contains(
            "let resultBytes = [UInt8](UnsafeBufferPointer(start: resultPtr, count: resultLen))"
        ),
        "callback must copy the borrowed buffer: {out}"
    );
    assert!(
        out.contains("var resultReader = WvReader(bytes: resultBytes)"),
        "callback must decode the buffer: {out}"
    );
    assert!(
        out.contains("contRef.value.resume(returning: v0)"),
        "callback must resume with the decoded value: {out}"
    );
    // The producer frees the buffer after the callback returns; the wrapper
    // must not free it.
    let cb_pos = out.find("fetch_ids_async").expect("async launcher present");
    assert!(
        !out[cb_pos..].contains("weaveffi_free_bytes"),
        "async wrapper must not free the borrowed result buffer: {out}"
    );
}

#[test]
fn swift_cross_module_struct() {
    let api = make_api(vec![
        Module {
            name: "types".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Name".to_string(),
                doc: None,
                fields: vec![field("value", TypeRef::StringUtf8)],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        },
        Module {
            name: "ops".to_string(),
            functions: vec![plain_fn(
                "get_name",
                vec![p("id", TypeRef::I32)],
                Some(TypeRef::Record("types.Name".to_string())),
            )],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        },
    ]);

    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");

    assert!(
        out.contains("-> Name"),
        "cross-module return type should use local name 'Name': {out}"
    );
    assert!(
        out.contains("wvReadName(&rvReader)"),
        "cross-module struct decode should use the local codec name: {out}"
    );
    assert!(
        !out.contains("types.Name"),
        "dot-qualified name should not appear in generated Swift code: {out}"
    );
}

#[test]
fn swift_nested_module_output() {
    let api = make_api(vec![Module {
        name: "parent".to_string(),
        functions: vec![plain_fn("outer_fn", vec![], Some(TypeRef::I32))],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![Module {
            name: "child".to_string(),
            functions: vec![plain_fn("inner_fn", vec![], Some(TypeRef::I32))],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }],
    }]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public enum Parent {"),
        "top-level module enum missing: {out}"
    );
    assert!(
        out.contains("public enum Child {"),
        "nested module enum missing: {out}"
    );
    assert!(
        out.contains("weaveffi_parent_outer_fn"),
        "parent C ABI call missing: {out}"
    );
    assert!(
        out.contains("weaveffi_parent_child_inner_fn"),
        "nested child C ABI call missing: {out}"
    );
}

/// A module with an `iter<i32>` function, throwing or not.
fn iter_api(throws: bool) -> Api {
    make_api(vec![Module {
        name: "data".to_string(),
        functions: vec![Function {
            name: "list_items".to_string(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
            doc: None,
            throws,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: if throws {
            Some(ErrorDomain {
                name: "DataError".to_string(),
                codes: vec![ErrorCode {
                    name: "Broken".to_string(),
                    code: 7,
                    message: "Broken".to_string(),
                    doc: None,
                    fields: vec![],
                }],
            })
        } else {
            None
        },
        modules: vec![],
    }])
}

#[test]
fn swift_iterator_emits_lazy_sequence_class() {
    let out = render(
        &iter_api(false),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    // A final class conforming to Sequence & IteratorProtocol owns the
    // handle; the wrapper returns it instead of a drained array.
    assert!(
        out.contains("public final class DataListItemsIterator: Sequence, IteratorProtocol {"),
        "missing lazy sequence class: {out}"
    );
    assert!(
        out.contains("public static func listItems() -> DataListItemsIterator {"),
        "wrapper must return the sequence type, not an array: {out}"
    );
    assert!(
        out.contains("return DataListItemsIterator(handle: rv)"),
        "wrapper must hand the launched handle to the sequence: {out}"
    );
    // No hidden drain: the wrapper body never loops over `_next`.
    assert!(
        !out.contains("while weaveffi_data_ListItemsIterator_next"),
        "wrapper must not drain the iterator eagerly: {out}"
    );
    assert!(
        !out.contains("-> [Int32]"),
        "iterator wrapper must not return an array: {out}"
    );
}

#[test]
fn swift_iterator_next_pulls_one_element_and_destroys() {
    let out = render(
        &iter_api(false),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    // Exactly one producer `next` call, inside `next()`.
    assert_eq!(
        out.matches("weaveffi_data_ListItemsIterator_next(").count(),
        1,
        "expected exactly one next call (inside next()): {out}"
    );
    assert!(
        out.contains("public func next() -> Int32? {"),
        "missing IteratorProtocol next(): {out}"
    );
    // Destroy happens eagerly on exhaustion and from deinit, guarded
    // against double-destroy by nulling the handle.
    assert!(
        out.contains("private func destroyHandle() {"),
        "missing destroy helper: {out}"
    );
    assert!(
        out.contains("guard let handle = handle else { return }")
            && out.contains("weaveffi_data_ListItemsIterator_destroy(handle)")
            && out.contains("self.handle = nil"),
        "destroy must null the handle to prevent double-destroy: {out}"
    );
    let deinit_pos = out.find("deinit {").expect("deinit present");
    assert!(
        out[deinit_pos..].contains("destroyHandle()"),
        "deinit must destroy an abandoned iterator: {out}"
    );
    // Non-throwing: a mid-stream error is a producer bug and traps.
    assert!(
        out.contains("fatalError(\"\\(code): \\(message)\")"),
        "non-throwing iterator must trap on per-next errors: {out}"
    );
    assert!(
        !out.contains("public private(set) var error"),
        "non-throwing iterator has no error property: {out}"
    );
}

#[test]
fn swift_throwing_iterator_stores_per_next_error() {
    let out = render(
        &iter_api(true),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    // Launch errors throw through the domain checker in the wrapper.
    assert!(
        out.contains("public static func listItems() throws -> DataListItemsIterator {"),
        "throwing wrapper must keep its throws signature: {out}"
    );
    let wrapper_pos = out
        .find("public static func listItems()")
        .expect("wrapper present");
    assert!(
        out[wrapper_pos..].contains("try checkData(&err)"),
        "launch errors must throw through the domain checker: {out}"
    );
    // Per-next domain errors end iteration and are stored on the sequence.
    assert!(
        out.contains("public private(set) var error: Error?"),
        "throwing iterator must expose the stored error: {out}"
    );
    assert!(
        out.contains("self.error = mapData(code: code, message: message, payload: payload)"),
        "per-next errors must map through the domain: {out}"
    );
    let class_pos = out
        .find("public final class DataListItemsIterator")
        .expect("class present");
    let class_body = &out[class_pos..];
    assert!(
        !class_body.contains("fatalError"),
        "throwing iterator must not trap on domain errors: {class_body}"
    );
}

#[test]
fn swift_string_iterator_frees_each_element() {
    let api = make_api(vec![Module {
        name: "data".to_string(),
        functions: vec![plain_fn(
            "list_names",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("let element = String(cString: item!)"),
        "string element must be copied: {out}"
    );
    assert!(
        out.contains("weaveffi_free_string(item)"),
        "string element must be freed after copying: {out}"
    );
}

#[test]
fn swift_record_iterator_decodes_and_frees_elements() {
    let api = make_api(vec![Module {
        name: "kv".to_string(),
        functions: vec![plain_fn(
            "scan",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
        )],
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
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("public func next() -> Entry? {"),
        "record iterator must yield the value type: {out}"
    );
    // A record element is a buffered element: two out slots, decode then
    // free with weaveffi_free_bytes.
    assert!(
        out.contains("var item: UnsafePointer<UInt8>? = nil")
            && out.contains("var itemLen: Int = 0"),
        "buffered element must use the (ptr, len) slots: {out}"
    );
    assert!(
        out.contains("wvReadEntry(&itemReader)"),
        "record element must decode through its codec: {out}"
    );
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: item), itemLen)"),
        "buffered element must be freed after copying: {out}"
    );
}

#[test]
fn list_return_frees_buffer_and_decodes_string_elements() {
    let api = make_api(vec![Module {
        name: "data".to_string(),
        functions: vec![
            plain_fn(
                "get_ids",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::I32))),
            ),
            plain_fn(
                "get_names",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
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
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // The whole encoding is one producer-allocated buffer, released once.
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)"),
        "list return must free the value buffer: {out}"
    );
    // String elements decode from the buffer; there is no per-element free.
    assert!(
        out.contains("rvReader.readString()"),
        "string list elements must decode from the buffer: {out}"
    );
    assert!(
        !out.contains("weaveffi_free_string(rv"),
        "no per-element string free remains: {out}"
    );
}

#[test]
fn map_return_decodes_single_buffer() {
    let api = make_api(vec![Module {
        name: "store".to_string(),
        functions: vec![plain_fn(
            "get_scores",
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
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)"),
        "map return must free the value buffer once: {out}"
    );
    assert!(
        out.contains("rvReader.readString()") && out.contains("rvReader.readInt32()"),
        "map entries must decode alternating key, value: {out}"
    );
    assert!(
        !out.contains("outKeysPtr") && !out.contains("weaveffi_free_string("),
        "no parallel arrays or per-key frees remain: {out}"
    );
}

#[test]
fn async_string_result_is_copied_not_freed() {
    let api = make_api(vec![Module {
        name: "tasks".to_string(),
        functions: vec![Function {
            name: "fetch".to_string(),
            params: vec![],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: true,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    // The callback's string is borrowed for the callback's duration: it
    // is copied into the resumed value and never freed by the wrapper.
    assert!(
        out.contains("contRef.value.resume(returning: String(cString: result))"),
        "async string result must be copied before resuming: {out}"
    );
    let cb_pos = out.find("fetch_async").expect("async launcher present");
    assert!(
        !out[cb_pos..].contains("weaveffi_free_string"),
        "async wrapper must not free the borrowed result buffer: {out}"
    );
}

#[test]
fn deprecated_function_generates_annotation() {
    let api = make_api(vec![Module {
        name: "math".to_string(),
        functions: vec![Function {
            name: "add_old".to_string(),
            params: vec![p("a", TypeRef::I32), p("b", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: Some("Use addV2 instead".to_string()),
            since: Some("0.1.0".to_string()),
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let out = render(&api, "weaveffi", true, "weaveffi.yml", "WeaveFFI.swift");
    assert!(
        out.contains("@available(*, deprecated, message: \"Use addV2 instead\")"),
        "missing deprecation annotation: {out}"
    );
    assert!(
        out.contains("func addOld("),
        "missing function declaration: {out}"
    );
}

fn doc_api() -> Api {
    make_api(vec![Module {
        name: "docs".into(),
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
        errors: Some(ErrorDomain {
            name: "DocsErrors".into(),
            codes: vec![ErrorCode {
                name: "not_found".into(),
                code: 1,
                message: "Not found".into(),
                doc: Some("Raised when missing".into()),
                fields: vec![],
            }],
        }),
        modules: vec![],
    }])
}

#[test]
fn swift_emits_doc_on_function() {
    let out = render(
        &doc_api(),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    assert!(out.contains("/// Performs a thing."), "{out}");
}

#[test]
fn swift_emits_doc_on_struct() {
    let out = render(
        &doc_api(),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    assert!(out.contains("/// An item we track."), "{out}");
}

#[test]
fn swift_emits_doc_on_enum_variant() {
    let out = render(
        &doc_api(),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    assert!(out.contains("/// Kind of item."), "{out}");
    assert!(out.contains("/// A small one"), "{out}");
}

#[test]
fn swift_emits_doc_on_field() {
    let out = render(
        &doc_api(),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    assert!(out.contains("/// Stable id"), "{out}");
}

#[test]
fn swift_emits_doc_on_param() {
    let out = render(
        &doc_api(),
        "weaveffi",
        true,
        "weaveffi.yml",
        "WeaveFFI.swift",
    );
    assert!(out.contains("/// - Parameter x: the input value"), "{out}");
}

/// The `shapes` sample: a rich (algebraic) enum `Shape` (a unit variant, an
/// f64 payload, two f32 payloads, and a string+u8 payload), a plain C-style
/// enum `Channel`, and the free functions that take/return `Shape` (lowered
/// to `TypeRef::RichEnum`) plus the numerics smoke `sum_bytes`.
fn shapes_api() -> Api {
    make_api(vec![Module {
        name: "shapes".into(),
        functions: vec![
            plain_fn(
                "describe",
                vec![p("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::StringUtf8),
            ),
            plain_fn(
                "scale",
                vec![
                    p("shape", TypeRef::RichEnum("Shape".into())),
                    p("factor", TypeRef::F64),
                ],
                Some(TypeRef::RichEnum("Shape".into())),
            ),
            plain_fn(
                "sum_bytes",
                vec![p("values", TypeRef::List(Box::new(TypeRef::U8)))],
                Some(TypeRef::U64),
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
                variants: vec![variant("Red", 0), variant("Green", 1)],
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
fn rich_enum_emits_native_swift_enum() {
    let out = render(
        &shapes_api(),
        "weaveffi",
        false,
        "shapes.yml",
        "Shapes.swift",
    );
    // A native Swift enum with associated values, one case per variant,
    // labeled by the field names.
    assert!(
        out.contains("public enum Shape {"),
        "missing native enum: {out}"
    );
    assert!(out.contains("case empty"), "missing unit case: {out}");
    assert!(
        out.contains("case circle(radius: Double)"),
        "missing single-payload case: {out}"
    );
    assert!(
        out.contains("case rectangle(width: Float, height: Float)"),
        "missing two-payload case: {out}"
    );
    assert!(
        out.contains("case labeled(label: String, count: UInt8)"),
        "missing mixed-payload case: {out}"
    );
    // No opaque wrapper class, no C symbols, no tag reader, no factories.
    assert!(
        !out.contains("public class Shape"),
        "rich enum must not be a class: {out}"
    );
    assert!(
        !out.contains("Shape_destroy") && !out.contains("Shape_tag"),
        "rich enum has no C symbols: {out}"
    );
    // The sibling plain C-style enum is still a raw-value Swift enum.
    assert!(
        out.contains("public enum Channel: UInt32 {"),
        "plain enum regressed: {out}"
    );
}

#[test]
fn rich_enum_emits_codec_pair() {
    let out = render(
        &shapes_api(),
        "weaveffi",
        false,
        "shapes.yml",
        "Shapes.swift",
    );
    assert!(
        out.contains("func wvWriteShape(_ value: Shape, into w: inout WvWriter) {"),
        "missing pack routine: {out}"
    );
    // The writer emits the i32 tag then the variant's fields in order.
    assert!(
        out.contains("case .empty:") && out.contains("w.writeInt32(0)"),
        "unit case must write only its tag: {out}"
    );
    assert!(
        out.contains("case let .circle(v0):")
            && out.contains("w.writeInt32(1)")
            && out.contains("w.writeDouble(v0)"),
        "payload case must write tag then fields: {out}"
    );
    assert!(
        out.contains("func wvReadShape(_ r: inout WvReader) -> Shape {"),
        "missing unpack routine: {out}"
    );
    assert!(
        out.contains("return .circle(radius:"),
        "unpack must rebuild the case with labels: {out}"
    );
    assert!(
        out.contains("wvDecodeFailure(\"unknown Shape tag"),
        "unpack must reject unknown tags: {out}"
    );
}

#[test]
fn rich_enum_functions_marshal_buffers() {
    let out = render(
        &shapes_api(),
        "weaveffi",
        false,
        "shapes.yml",
        "Shapes.swift",
    );
    // describe(Shape) -> String: packs the enum, frees the returned string.
    assert!(
        out.contains("public static func shapesDescribe(shape: Shape) -> String {"),
        "missing describe signature: {out}"
    );
    assert!(
        out.contains("wvWriteShape(shape, into: &shapeWriter)"),
        "describe must pack the enum: {out}"
    );
    assert!(
        out.contains("weaveffi_shapes_describe(shape_ptr, shape_len, &err)"),
        "describe must pass the buffer pair: {out}"
    );
    // scale(Shape, f64) -> Shape: buffer in, buffer out.
    assert!(
        out.contains("public static func shapesScale(shape: Shape, factor: Double) -> Shape {"),
        "missing scale signature: {out}"
    );
    assert!(
        out.contains("weaveffi_shapes_scale(shape_ptr, shape_len, factor, &outLen, &err)"),
        "scale must pass buffer pair plus scalar: {out}"
    );
    assert!(
        out.contains("wvReadShape(&rvReader)"),
        "scale must decode the returned buffer: {out}"
    );
    // sum_bytes([u8]) -> u64: numerics smoke.
    assert!(
        out.contains("public static func shapesSumBytes(values: [UInt8]) -> UInt64 {"),
        "missing sum_bytes signature: {out}"
    );
}

/// A `kv` module with a declared error domain and a `Store` interface
/// exercising every member kind: a plain constructor named `new`, a
/// throwing factory constructor, throwing and non-throwing methods, an
/// async throwing method, and a static.
fn store_api() -> Api {
    use weaveffi_ir::ir::InterfaceDef;
    fn f(
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
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }
    make_api(vec![Module {
        name: "kv".into(),
        functions: vec![f(
            "inspect",
            vec![p("store", TypeRef::Interface("Store".into()))],
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
                f("new", vec![p("capacity", TypeRef::I64)], None, false, false),
                f(
                    "open",
                    vec![p("path", TypeRef::StringUtf8)],
                    None,
                    true,
                    false,
                ),
            ],
            methods: vec![
                f(
                    "put",
                    vec![
                        p("key", TypeRef::StringUtf8),
                        p("value", TypeRef::StringUtf8),
                    ],
                    None,
                    true,
                    false,
                ),
                f("count", vec![], Some(TypeRef::I64), false, false),
                f("compact", vec![], Some(TypeRef::I64), true, true),
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
            codes: vec![ErrorCode {
                name: "KeyNotFound".into(),
                code: 1001,
                message: "Key not found".into(),
                doc: None,
                fields: vec![],
            }],
        }),
        modules: vec![],
    }])
}

#[test]
fn interface_emits_final_class_with_deinit() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    assert!(
        out.contains("public final class Store {"),
        "missing final class: {out}"
    );
    assert!(
        out.contains("let ptr: OpaquePointer"),
        "missing handle property: {out}"
    );
    assert!(
        out.contains("init(ptr: OpaquePointer) {"),
        "missing ownership-adopting init: {out}"
    );
    assert!(
        out.contains("deinit {\n        weaveffi_kv_Store_destroy(ptr)"),
        "deinit must call the destroy symbol: {out}"
    );
}

#[test]
fn interface_ctor_new_becomes_init() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    // Non-throwing `new` with a labeled parameter.
    assert!(
        out.contains("public init(capacity: Int64) {"),
        "missing public init: {out}"
    );
    assert!(
        out.contains("let rv = weaveffi_kv_Store_new(capacity, &err)"),
        "init must call the constructor symbol: {out}"
    );
    assert!(
        out.contains("self.ptr = rv"),
        "init must adopt the returned handle: {out}"
    );
}

#[test]
fn interface_secondary_ctor_is_throwing_factory() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    assert!(
        out.contains("public static func open(path: String) throws -> Store {"),
        "missing factory signature: {out}"
    );
    assert!(
        out.contains("let rv: OpaquePointer? = path.withCString { path_ptr in"),
        "factory must stage the string param: {out}"
    );
    assert!(
        out.contains("return weaveffi_kv_Store_open(path_ptr, &err)"),
        "factory must call the constructor symbol: {out}"
    );
    assert!(
        out.contains("try checkKv(&err)"),
        "throwing factory must use the domain checker: {out}"
    );
    assert!(
        out.contains("return Store(ptr: rv)"),
        "factory must wrap the owned pointer: {out}"
    );
}

#[test]
fn interface_methods_pass_self_pointer() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    // Throwing instance method with labeled string params: `ptr` leads
    // the C argument list.
    assert!(
        out.contains("public func put(key: String, value: String) throws -> Void {"),
        "missing throwing method: {out}"
    );
    assert!(
        out.contains("weaveffi_kv_Store_put(ptr, key_ptr, value_ptr, &err)"),
        "method must pass ptr as the leading C argument: {out}"
    );
    // Non-throwing instance method traps instead.
    assert!(
        out.contains("public func count() -> Int64 {"),
        "missing plain method: {out}"
    );
    let count_body = &out[out.find("public func count()").expect("count body")..];
    assert!(
        count_body.contains("weaveffi_kv_Store_count(ptr, &err)")
            && count_body.contains("trap(&err)"),
        "plain method must call with ptr and trap: {out}"
    );
}

#[test]
fn interface_async_method_is_async_throws() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    assert!(
        out.contains("public func compact() async throws -> Int64 {"),
        "missing async throws method: {out}"
    );
    assert!(
        out.contains("weaveffi_kv_Store_compact_async(ptr, {"),
        "async launcher must lead with ptr: {out}"
    );
    assert!(
        out.contains("resume(throwing: mapKv(code: code, message: msg, payload: payload))"),
        "async method must resume with the typed domain error: {out}"
    );
}

#[test]
fn interface_static_is_static_func() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    assert!(
        out.contains("public static func defaultCapacity() -> Int64 {"),
        "missing static func: {out}"
    );
    assert!(
        out.contains("weaveffi_kv_Store_default_capacity(&err)"),
        "static must call its member symbol: {out}"
    );
}

#[test]
fn interface_param_passes_borrowed_pointer() {
    let out = render(&store_api(), "weaveffi", true, "kv.yml", "Kv.swift");
    // Free function taking the interface: the class is the Swift type and
    // the call borrows its handle.
    assert!(
        out.contains("public static func inspect(store: Store) -> Int64 {"),
        "missing interface-typed param signature: {out}"
    );
    assert!(
        out.contains("weaveffi_kv_inspect(store.ptr, &err)"),
        "interface param must pass .ptr: {out}"
    );
}

#[test]
fn throws_split_on_free_functions() {
    let api = make_api(vec![Module {
        name: "calc".into(),
        functions: vec![
            Function {
                name: "div".into(),
                params: vec![p("a", TypeRef::I32), p("b", TypeRef::I32)],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: true,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            plain_fn(
                "add",
                vec![p("a", TypeRef::I32), p("b", TypeRef::I32)],
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
    let out = render(&api, "weaveffi", true, "calc.yml", "Calc.swift");
    // throws: true -> `throws` signature checked through the domain.
    assert!(
        out.contains("public static func div(a: Int32, b: Int32) throws -> Int32 {"),
        "missing throwing signature: {out}"
    );
    let div_body = &out[out.find("func div(").expect("div body")..];
    assert!(
        div_body.contains("try checkCalc(&err)"),
        "throwing fn must use the domain checker: {out}"
    );
    // throws: false -> plain signature; the slot still traps.
    assert!(
        out.contains("public static func add(a: Int32, b: Int32) -> Int32 {"),
        "missing plain signature: {out}"
    );
    let add_body = &out[out.find("func add(").expect("add body")..];
    assert!(
        add_body.contains("trap(&err)"),
        "plain fn must trap on a reported error: {out}"
    );
    // The trapping helper is the fatalError path.
    assert!(
        out.contains("fatalError(\"\\(code): \\(message)\")"),
        "trap helper must fatalError: {out}"
    );
}

#[test]
fn strip_module_prefix_defaults_to_true() {
    assert!(
        SwiftConfig::default().strip_module_prefix,
        "stripping must be the default"
    );
    // The default config produces stripped, camel-cased names end to end.
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![plain_fn(
            "create_contact",
            vec![p("display_name", TypeRef::StringUtf8)],
            Some(TypeRef::I32),
        )],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let model = BindingModel::build(&api, "weaveffi");
    let files = LanguageBackend::files(
        &SwiftGenerator,
        &api,
        &model,
        Utf8Path::new("/out"),
        &SwiftConfig::default(),
    );
    let wrapper = files
        .iter()
        .find(|f| f.path.as_str().ends_with("WeaveFFI.swift"))
        .expect("wrapper file");
    assert!(
        wrapper
            .contents
            .contains("public static func createContact(displayName: String) -> Int32 {"),
        "default must strip the module prefix and camel the parameter: {}",
        wrapper.contents
    );
    assert!(
        !wrapper.contents.contains("contactsCreateContact"),
        "default must not emit prefixed names: {}",
        wrapper.contents
    );
}
