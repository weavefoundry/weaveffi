//! Unit tests: golden-content assertions over every emitted file.

use super::*;
use crate::addon::render_addon_c;
use crate::entities::render_node_index;
use crate::package::render_node_dts;
use crate::types::ts_type_for;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef, Module, Param,
    StructDef, StructField, TypeRef,
};

#[test]
fn package_uses_optional_dependencies_per_platform() {
    use camino::Utf8Path;
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = make_api(vec![make_module("calc")]);
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &NodeGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &NodeConfig::default(),
    )
    .expect("node supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    let main = files
        .iter()
        .find(|f| f.path.as_str().ends_with("node/package.json"))
        .expect("main package.json present");
    let FileContent::Text(pkg) = &main.content else {
        panic!("package.json is text");
    };
    assert!(pkg.contains("\"optionalDependencies\""));
    assert!(pkg.contains("weaveffi-darwin-arm64") && pkg.contains("weaveffi-win32-x64"));
    // The per-platform native package is gated by npm os/cpu.
    let plat = files
        .iter()
        .find(|f| {
            f.path
                .as_str()
                .ends_with("npm/weaveffi-win32-x64/package.json")
        })
        .expect("platform package present");
    let FileContent::Text(pp) = &plat.content else {
        panic!("platform package.json is text");
    };
    assert!(
        pp.contains("\"os\": [\"win32\"]") && pp.contains("\"cpu\": [\"x64\"]"),
        "os/cpu gating missing: {pp}"
    );
}

fn make_api(modules: Vec<Module>) -> Api {
    Api {
        version: "0.6.0".into(),
        modules,
        generators: None,
        package: None,
    }
}

fn make_module(name: &str) -> Module {
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

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
        default: None,
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

fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>, throws: bool) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        r#async: false,
        cancellable: false,
        throws,
        deprecated: None,
        since: None,
    }
}

/// A `Contact { name: string, age: i32 }` record for buffered-type tests.
fn contact_struct() -> StructDef {
    StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![
            field("name", TypeRef::StringUtf8),
            field("age", TypeRef::I32),
        ],
    }
}

/// Test-only bridge from an inline [`Api`] literal to the model the
/// production path receives from the driver.
fn build_model(api: &Api) -> BindingModel {
    BindingModel::build(api, "weaveffi")
}

fn index_for(api: &Api, strip: bool) -> String {
    render_node_index(&build_model(api), strip, "weaveffi.yml")
}

fn dts_for(api: &Api, strip: bool) -> String {
    render_node_dts(&build_model(api), strip, "weaveffi.yml")
}

fn addon_for(api: &Api, strip: bool) -> String {
    render_addon_c(&build_model(api), strip, "weaveffi.yml")
}

#[test]
fn listeners_generate_tsfn_register_unregister() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnMessage".into(),
            doc: None,
            params: vec![param("message", TypeRef::StringUtf8)],
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    NodeGenerator
        .generate(&api, out, &NodeConfig::default())
        .unwrap();
    let addon = std::fs::read_to_string(dir.path().join("node/weaveffi_addon.c")).unwrap();
    assert!(
        addon.contains("napi_create_threadsafe_function"),
        "listeners must use threadsafe functions: {addon}"
    );
    assert!(
        addon.contains("Napi_weaveffi_events_register_message_listener"),
        "register N-API fn missing: {addon}"
    );
    assert!(
        addon.contains("Napi_weaveffi_events_unregister_message_listener"),
        "unregister N-API fn missing: {addon}"
    );
    assert!(
        addon.contains("napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking)"),
        "trampoline must queue payloads: {addon}"
    );
    assert!(
        addon.contains("napi_unref_threadsafe_function"),
        "tsfn must be unref'd so listeners don't pin the loop: {addon}"
    );
    let dts = std::fs::read_to_string(dir.path().join("node/types.d.ts")).unwrap();
    assert!(
        dts.contains(
            "export function registerMessageListener(callback: (message: string) => void): number"
        ),
        "register dts missing: {dts}"
    );
    assert!(
        dts.contains("export function unregisterMessageListener(id: number): void"),
        "unregister dts missing: {dts}"
    );
}

#[test]
fn ts_type_for_primitives() {
    assert_eq!(ts_type_for(&TypeRef::I32), "number");
    assert_eq!(ts_type_for(&TypeRef::Bool), "boolean");
    assert_eq!(ts_type_for(&TypeRef::StringUtf8), "string");
    assert_eq!(ts_type_for(&TypeRef::Bytes), "Buffer");
    assert_eq!(ts_type_for(&TypeRef::Handle), "bigint");
}

#[test]
fn ts_type_for_struct_and_enum() {
    assert_eq!(ts_type_for(&TypeRef::Record("Contact".into())), "Contact");
    assert_eq!(ts_type_for(&TypeRef::Enum("Color".into())), "Color");
    assert_eq!(
        ts_type_for(&TypeRef::TypedHandle("Contact".into())),
        "Contact"
    );
}

#[test]
fn ts_type_for_cross_module_uses_local_name() {
    // A typed handle resolved to a parent-module struct (`kv.Store`) must
    // emit the bare local interface name, the only TS type in this module.
    assert_eq!(
        ts_type_for(&TypeRef::TypedHandle("kv.Store".into())),
        "Store"
    );
    assert_eq!(ts_type_for(&TypeRef::Record("kv.Store".into())), "Store");
    assert_eq!(ts_type_for(&TypeRef::Enum("kv.Kind".into())), "Kind");
}

#[test]
fn ts_type_for_optional() {
    let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
    assert_eq!(ts_type_for(&ty), "string | null");
}

#[test]
fn ts_type_for_list() {
    let ty = TypeRef::List(Box::new(TypeRef::I32));
    assert_eq!(ts_type_for(&ty), "number[]");
}

#[test]
fn ts_type_for_list_of_optional() {
    let ty = TypeRef::List(Box::new(TypeRef::Optional(Box::new(TypeRef::I32))));
    assert_eq!(ts_type_for(&ty), "(number | null)[]");
}

#[test]
fn ts_type_for_map() {
    let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
    assert_eq!(ts_type_for(&ty), "Record<string, number>");
}

#[test]
fn ts_type_for_optional_list() {
    let ty = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::I32))));
    assert_eq!(ts_type_for(&ty), "number[] | null");
}

#[test]
fn generate_node_dts_with_structs() {
    let mut m = make_module("contacts");
    m.structs.push(StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![
            field("name", TypeRef::StringUtf8),
            field("age", TypeRef::I32),
            field("active", TypeRef::Bool),
        ],
    });
    m.enums.push(EnumDef {
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
            EnumVariant {
                name: "Blue".into(),
                value: 2,
                doc: None,
                fields: vec![],
            },
        ],
    });
    m.functions.push(func(
        "get_contact",
        vec![param("id", TypeRef::I32)],
        Some(TypeRef::Optional(Box::new(TypeRef::Record(
            "Contact".into(),
        )))),
        false,
    ));
    m.functions.push(func(
        "list_contacts",
        vec![],
        Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
        false,
    ));

    let dts = dts_for(&make_api(vec![m]), true);

    assert!(dts.contains("export interface Contact {"));
    assert!(dts.contains("  name: string;"));
    assert!(dts.contains("  age: number;"));
    assert!(dts.contains("  active: boolean;"));
    assert!(dts.contains("export enum Color {"));
    assert!(dts.contains("  Red = 0,"));
    assert!(dts.contains("  Green = 1,"));
    assert!(dts.contains("  Blue = 2,"));
    assert!(dts.contains("export function getContact(id: number): Contact | null"));
    assert!(dts.contains("export function listContacts(): Contact[]"));

    let iface_pos = dts.find("export interface Contact").unwrap();
    let enum_pos = dts.find("export enum Color").unwrap();
    let fn_pos = dts.find("export function getContact").unwrap();
    assert!(
        iface_pos < fn_pos,
        "interface should appear before functions"
    );
    assert!(enum_pos < fn_pos, "enum should appear before functions");
}

#[test]
fn node_generates_binding_gyp() {
    let api = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_node_binding_gyp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    NodeGenerator
        .generate(&api, out_dir, &NodeConfig::default())
        .unwrap();

    let gyp = std::fs::read_to_string(tmp.join("node").join("binding.gyp")).unwrap();
    assert!(
        gyp.contains("\"target_name\": \"weaveffi\""),
        "missing target_name: {gyp}"
    );
    assert!(
        gyp.contains("weaveffi_addon.c"),
        "missing source file: {gyp}"
    );

    let addon = std::fs::read_to_string(tmp.join("node").join("weaveffi_addon.c")).unwrap();
    assert!(
        addon.contains("napi_value Init("),
        "missing Init function: {addon}"
    );
    assert!(
        addon.contains("weaveffi_math_add"),
        "missing C ABI call: {addon}"
    );
    assert!(
        addon.contains("napi_get_cb_info"),
        "missing napi_get_cb_info call: {addon}"
    );

    let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
    assert!(pkg.contains("\"gypfile\": true"), "missing gypfile: {pkg}");
    assert!(
        pkg.contains("node-gyp rebuild"),
        "missing install script: {pkg}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_node_dts_with_structs_and_enums() {
    let api = make_api(vec![Module {
        name: "contacts".to_string(),
        functions: vec![
            func(
                "get_contact",
                vec![param("id", TypeRef::I32)],
                Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                false,
            ),
            func(
                "list_contacts",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                false,
            ),
            func(
                "set_favorite_color",
                vec![
                    param("contact_id", TypeRef::I32),
                    param(
                        "color",
                        TypeRef::Optional(Box::new(TypeRef::Enum("Color".into()))),
                    ),
                ],
                None,
                false,
            ),
            func(
                "get_tags",
                vec![param("contact_id", TypeRef::I32)],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                false,
            ),
        ],
        structs: vec![StructDef {
            name: "Contact".to_string(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                field("tags", TypeRef::List(Box::new(TypeRef::StringUtf8))),
            ],
        }],
        enums: vec![EnumDef {
            name: "Color".to_string(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".to_string(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".to_string(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Blue".to_string(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_node_structs_and_enums");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    NodeGenerator
        .generate(
            &api,
            out_dir,
            &NodeConfig {
                strip_module_prefix: true,
                ..NodeConfig::default()
            },
        )
        .unwrap();

    let dts = std::fs::read_to_string(tmp.join("node").join("types.d.ts")).unwrap();

    assert!(
        dts.contains("export interface Contact {"),
        "missing Contact interface: {dts}"
    );
    assert!(dts.contains("  name: string;"), "missing name field: {dts}");
    assert!(
        dts.contains("  email: string | null;"),
        "missing optional email field: {dts}"
    );
    assert!(
        dts.contains("  tags: string[];"),
        "missing list tags field: {dts}"
    );

    assert!(
        dts.contains("export enum Color {"),
        "missing Color enum: {dts}"
    );
    assert!(dts.contains("  Red = 0,"), "missing Red variant: {dts}");
    assert!(dts.contains("  Green = 1,"), "missing Green variant: {dts}");
    assert!(dts.contains("  Blue = 2,"), "missing Blue variant: {dts}");

    assert!(
        dts.contains("export function getContact(id: number): Contact | null"),
        "missing getContact with optional return: {dts}"
    );
    assert!(
        dts.contains("export function listContacts(): Contact[]"),
        "missing listContacts with list return: {dts}"
    );
    assert!(
        dts.contains(
            "export function setFavoriteColor(contactId: number, color: Color | null): void"
        ),
        "missing setFavoriteColor with optional enum param: {dts}"
    );
    assert!(
        dts.contains("export function getTags(contactId: number): string[]"),
        "missing getTags with list return: {dts}"
    );

    let iface_pos = dts.find("export interface Contact").unwrap();
    let enum_pos = dts.find("export enum Color").unwrap();
    let fn_pos = dts.find("export function getContact").unwrap();
    assert!(
        iface_pos < fn_pos,
        "interface should appear before functions"
    );
    assert!(enum_pos < fn_pos, "enum should appear before functions");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn node_custom_package_name() {
    let api = make_api(vec![make_module("math")]);

    let tmp = std::env::temp_dir().join("weaveffi_test_node_custom_pkg");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    let config = NodeConfig {
        package_name: Some("@myorg/cool-lib".into()),
        ..NodeConfig::default()
    };
    NodeGenerator.generate(&api, out_dir, &config).unwrap();

    let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
    assert!(
        pkg.contains("\"name\": \"@myorg/cool-lib\""),
        "package.json should use custom name: {pkg}"
    );
    assert!(
        !pkg.contains("\"name\": \"weaveffi\""),
        "package.json should not contain default name: {pkg}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn node_dts_has_jsdoc() {
    let api = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m.functions.push(func(
            "subtract",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);

    let dts = dts_for(&api, true);

    assert!(
        dts.contains("Maps to C function: weaveffi_math_add"),
        "missing JSDoc for add: {dts}"
    );
    assert!(
        dts.contains("Maps to C function: weaveffi_math_subtract"),
        "missing JSDoc for subtract: {dts}"
    );
}

#[test]
fn node_addon_has_no_todo() {
    let api = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        !addon.contains("// TODO: implement"),
        "generated addon.c should not contain TODO comments: {addon}"
    );
}

#[test]
fn node_addon_extracts_args() {
    let api = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("napi_get_cb_info"),
        "generated addon.c should call napi_get_cb_info: {addon}"
    );
}

#[test]
fn node_addon_frees_strings() {
    let api = make_api(vec![{
        let mut m = make_module("greet");
        m.functions.push(func(
            "hello",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("weaveffi_free_string(result)"),
        "generated addon should free returned strings: {addon}"
    );
    assert!(
        addon.contains("#include <string.h>"),
        "generated addon should include string.h: {addon}"
    );
    assert!(
        addon.contains("#include <stdlib.h>"),
        "generated addon should include stdlib.h: {addon}"
    );
    assert!(
        addon.contains("weaveffi_error_clear(&err)"),
        "generated addon should clear errors: {addon}"
    );
}

#[test]
fn node_custom_prefix_threads_to_user_symbols() {
    let api = make_api(vec![{
        let mut m = make_module("greet");
        m.functions.push(func(
            "hello",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
            false,
        ));
        m
    }]);

    let config = NodeConfig {
        prefix: Some("myffi".into()),
        ..NodeConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_node_custom_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    NodeGenerator.generate(&api, out_dir, &config).unwrap();

    // The output file name is a fixed library artifact name, not the ABI
    // prefix, so it stays `weaveffi_addon.c` regardless of `prefix`.
    let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();

    // User symbols pick up the configured ABI prefix.
    assert!(
        addon.contains("myffi_greet_hello"),
        "addon should call the prefixed user symbol myffi_greet_hello: {addon}"
    );
    assert!(
        !addon.contains("weaveffi_greet_hello"),
        "addon must not emit the hard-coded weaveffi_ user symbol: {addon}"
    );
    assert!(
        addon.contains("#include \"myffi.h\""),
        "addon should include the prefixed header myffi.h: {addon}"
    );

    // Runtime ABI helpers are supplied by weaveffi-abi and stay literal.
    assert!(
        addon.contains("weaveffi_error"),
        "runtime weaveffi_error must remain literal: {addon}"
    );
    assert!(
        addon.contains("weaveffi_free_string"),
        "runtime weaveffi_free_string must remain literal: {addon}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn node_addon_checks_error() {
    let api = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("err.code"),
        "generated addon.c should check err.code: {addon}"
    );
}

#[test]
fn node_strip_module_prefix() {
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.functions.push(func(
            "create_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);

    let config = NodeConfig {
        strip_module_prefix: true,
        ..NodeConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_node_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    NodeGenerator.generate(&api, out_dir, &config).unwrap();

    let dts = std::fs::read_to_string(tmp.join("node/types.d.ts")).unwrap();
    assert!(
        dts.contains("export function createContact("),
        "stripped name should be createContact: {dts}"
    );
    assert!(
        !dts.contains("export function contactsCreateContact("),
        "should not contain module-prefixed name: {dts}"
    );

    let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();
    assert!(
        addon.contains("\"createContact\""),
        "JS export name should be stripped: {addon}"
    );
    assert!(
        addon.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {addon}"
    );

    // Stripping is the default; `strip_module_prefix: false` restores
    // module-prefixed (still lowerCamelCase) names.
    let default_cfg = NodeConfig::default();
    assert!(
        default_cfg.strip_module_prefix,
        "stripping must be the default"
    );
    let no_strip = NodeConfig {
        strip_module_prefix: false,
        ..NodeConfig::default()
    };
    let tmp2 = std::env::temp_dir().join("weaveffi_test_node_no_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp2);
    std::fs::create_dir_all(&tmp2).unwrap();
    let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

    NodeGenerator.generate(&api, out_dir2, &no_strip).unwrap();

    let dts2 = std::fs::read_to_string(tmp2.join("node/types.d.ts")).unwrap();
    assert!(
        dts2.contains("export function contactsCreateContact("),
        "opting out should restore module-prefixed names: {dts2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&tmp2);
}

#[test]
fn node_typed_handle_type() {
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "get_info",
            vec![param("contact", TypeRef::TypedHandle("Contact".into()))],
            None,
            false,
        ));
        m
    }]);
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("contact: Contact"),
        "TypedHandle should use class type not bigint: {dts}"
    );
}

#[test]
fn node_deeply_nested_optional() {
    let api = make_api(vec![{
        let mut m = make_module("edge");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "process",
            vec![param(
                "data",
                TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                    Box::new(TypeRef::Record("Contact".into())),
                ))))),
            )],
            None,
            false,
        ));
        m
    }]);
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("(Contact | null)[] | null"),
        "should contain deeply nested optional type: {dts}"
    );
}

#[test]
fn node_map_of_lists() {
    let api = make_api(vec![{
        let mut m = make_module("edge");
        m.functions.push(func(
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
        ));
        m
    }]);
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("Record<string, number[]>"),
        "should contain map of lists type: {dts}"
    );
}

#[test]
fn node_enum_keyed_map() {
    let api = make_api(vec![{
        let mut m = make_module("edge");
        m.structs.push(contact_struct());
        m.enums.push(EnumDef {
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
        });
        m.functions.push(func(
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
        ));
        m
    }]);
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("Record<Color, Contact>"),
        "should contain enum-keyed map type: {dts}"
    );
    // The wrapper packs the map with an enum key writer (JS object keys
    // arrive as strings, so the key coerces through Number) and the
    // record's pack function per value.
    let index = index_for(&api, true);
    assert!(
        index.contains(
            "__encode((w, v) => __wMap(w, v, (w, k) => w.i32(Number(k)), __packContact), contacts)"
        ),
        "map param must pack keys and values: {index}"
    );
}

#[test]
fn node_no_double_free_on_error() {
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "find_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Record("Contact".into())),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("free(name)"),
        "malloc'd JS string copy should be freed after the C call: {addon}"
    );
    assert!(
        !addon.contains("weaveffi_free_string(name)"),
        "input string param must not use weaveffi_free_string: {addon}"
    );
    let free_pos = addon
        .find("free(name)")
        .expect("free(name) should be present");
    let err_pos = addon
        .find("if (err.code != 0)")
        .expect("err.code check should be present");
    assert!(
        free_pos < err_pos,
        "cleanup should run before error check: free at {free_pos}, err at {err_pos}"
    );
    let err_block_start = addon
        .find("  if (err.code != 0) {\n")
        .expect("error if block should be present");
    let after_err = &addon[err_block_start..];
    let err_block_end_rel = after_err
        .find("  }\n  napi_value ret;")
        .expect("napi_value ret should follow error block");
    let err_block = &addon[err_block_start..err_block_start + err_block_end_rel];
    assert!(
        !err_block.contains("result"),
        "error path should not touch result before return NULL: {err_block}"
    );
    // The buffered record return is copied into a JS Buffer, then the
    // native encoding is released exactly once.
    assert!(
        addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
        "buffered return must be freed after copying: {addon}"
    );
}

#[test]
fn node_null_check_on_optional_interface_return() {
    // `Interface?` is the one optional that stays a nullable pointer at
    // the ABI (every other optional is buffered), so the addon must
    // null-check before surfacing the handle.
    let api = make_api(vec![{
        let mut m = make_module("kv");
        m.interfaces.push(InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![func("new", vec![], None, false)],
            methods: vec![],
            statics: vec![],
        });
        m.functions.push(func(
            "maybe_open",
            vec![param("path", TypeRef::StringUtf8)],
            Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                "Store".into(),
            )))),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("if (result == NULL)"),
        "optional interface return should null-check before wrapping: {addon}"
    );
    assert!(
        addon.contains("napi_get_null"),
        "optional absent should return JS null via napi_get_null: {addon}"
    );
    let index = index_for(&api, true);
    assert!(
        index.contains("return _r == null ? null : Store._fromHandle(_r);"),
        "the wrapper must null-check before wrapping the handle: {index}"
    );
}

#[test]
fn node_async_returns_promise() {
    let api = make_api(vec![{
        let mut m = make_module("tasks");
        m.functions.push(Function {
            name: "run".into(),
            params: vec![param("id", TypeRef::I32)],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m.functions.push(Function {
            name: "fire_and_forget".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m
    }]);
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("Promise<"),
        "async function should return Promise in .d.ts: {dts}"
    );
    assert!(
        dts.contains("): Promise<string>"),
        "async string return should be Promise<string>: {dts}"
    );
    assert!(
        dts.contains("): Promise<void>"),
        "async void return should be Promise<void>: {dts}"
    );
}

#[test]
fn node_addon_creates_promise() {
    let api = make_api(vec![{
        let mut m = make_module("tasks");
        m.functions.push(Function {
            name: "run".into(),
            params: vec![param("id", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("napi_create_promise"),
        "async addon should call napi_create_promise: {addon}"
    );
    assert!(
        addon.contains("napi_resolve_deferred"),
        "async callback should call napi_resolve_deferred: {addon}"
    );
    assert!(
        addon.contains("napi_reject_deferred"),
        "async callback should call napi_reject_deferred: {addon}"
    );
    assert!(
        addon.contains("weaveffi_tasks_run_napi_actx"),
        "async addon should define per-fn async context struct: {addon}"
    );
    assert!(
        addon.contains("weaveffi_tasks_run_async("),
        "async addon should call the _async C function: {addon}"
    );
    assert!(
        addon.contains("weaveffi_tasks_run_napi_cb"),
        "async addon should define the callback: {addon}"
    );
    // The completion callback may fire on any producer thread, so it must
    // queue through a threadsafe function instead of touching napi_env.
    assert!(
        addon.contains("napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking)"),
        "completion callback must hop to the JS thread via tsfn: {addon}"
    );
    assert!(
        !addon.contains("napi_resolve_deferred(ctx->env"),
        "deferred must never be settled from the producer thread: {addon}"
    );
    // A rejection carries the copied structured payload.
    assert!(
        addon.contains("ctx->err_payload = (uint8_t*)malloc(err->payload_len)"),
        "the error payload must be copied inside the callback: {addon}"
    );
    assert!(
        addon.contains(
            "weaveffi_napi_error_value(env, ctx->err_code, ctx->err_msg, ctx->err_payload, ctx->err_payload_len)"
        ),
        "the rejection must carry the copied payload: {addon}"
    );
}

/// The N-API deferred is created with `napi_create_promise` and settled
/// (on the JS thread) by exactly one of `napi_resolve_deferred` /
/// `napi_reject_deferred`. The per-fn async context that carries the
/// deferred + threadsafe function across threads must be allocated once
/// and freed exactly once, and the tsfn released exactly once.
#[test]
fn node_async_pins_callback_for_lifetime() {
    let api = make_api(vec![{
        let mut m = make_module("tasks");
        m.functions.push(Function {
            name: "run".into(),
            params: vec![param("id", TypeRef::I32)],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m
    }]);
    let addon = addon_for(&api, true);
    let create_count = addon.matches("napi_create_promise").count();
    let resolve_count = addon.matches("napi_resolve_deferred").count();
    let reject_count = addon.matches("napi_reject_deferred").count();
    let alloc_count = addon
        .matches("calloc(1, sizeof(weaveffi_tasks_run_napi_actx))")
        .count();
    let free_count = addon.matches("free(ctx);").count();
    let release_count = addon
        .matches("napi_release_threadsafe_function(ctx->tsfn, napi_tsfn_release);")
        .count();
    assert_eq!(
        create_count, 1,
        "expected one napi_create_promise per async fn, got {create_count}: {addon}"
    );
    assert_eq!(
        resolve_count, 1,
        "expected one napi_resolve_deferred per async fn, got {resolve_count}: {addon}"
    );
    assert_eq!(
        reject_count, 1,
        "expected one napi_reject_deferred per async fn, got {reject_count}: {addon}"
    );
    assert_eq!(
        alloc_count, free_count,
        "ctx alloc / free must balance per async fn: alloc={alloc_count} free={free_count}: {addon}"
    );
    assert_eq!(
        release_count, 1,
        "tsfn must be released exactly once per async fn, got {release_count}: {addon}"
    );
}

fn doc_module() -> Module {
    Module {
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
            r#async: false,
            cancellable: false,
            throws: false,
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
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }
}

#[test]
fn node_emits_doc_on_function() {
    let dts = dts_for(&make_api(vec![doc_module()]), true);
    assert!(dts.contains("Performs a thing."), "{dts}");
}

#[test]
fn node_emits_doc_on_struct() {
    let dts = dts_for(&make_api(vec![doc_module()]), true);
    assert!(dts.contains("/** An item we track. */"), "{dts}");
}

#[test]
fn node_emits_doc_on_enum_variant() {
    let dts = dts_for(&make_api(vec![doc_module()]), true);
    assert!(dts.contains("/** Kind of item. */"), "{dts}");
    assert!(dts.contains("/** A small one */"), "{dts}");
}

#[test]
fn node_emits_doc_on_field() {
    let dts = dts_for(&make_api(vec![doc_module()]), true);
    assert!(dts.contains("/** Stable id */"), "{dts}");
}

#[test]
fn node_emits_doc_on_param() {
    let dts = dts_for(&make_api(vec![doc_module()]), true);
    assert!(dts.contains("@param x the input value"), "{dts}");
}

// --- Value buffers -------------------------------------------------------

#[test]
fn buffer_runtime_emitted_only_when_needed() {
    // A scalar-only model carries no buffer runtime.
    let plain = make_api(vec![{
        let mut m = make_module("math");
        m.functions.push(func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
            false,
        ));
        m
    }]);
    let index = index_for(&plain, true);
    assert!(
        !index.contains("class __Writer"),
        "scalar-only model must not embed the buffer runtime: {index}"
    );

    // Declaring a record pulls in the writer, reader, and combinators.
    let buffered = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "save",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            None,
            false,
        ));
        m
    }]);
    let index = index_for(&buffered, true);
    for piece in [
        "class __Writer",
        "class __Reader",
        "function __wOpt",
        "function __rList",
        "function __wMap",
    ] {
        assert!(
            index.contains(piece),
            "buffer runtime piece `{piece}` missing: {index}"
        );
    }
}

#[test]
fn record_params_and_returns_use_value_buffers() {
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "save",
            vec![param("contact", TypeRef::Record("Contact".into()))],
            Some(TypeRef::Record("Contact".into())),
            false,
        ));
        m
    }]);

    // Addon: the record param crosses as the borrowed (ptr, len) pair the
    // JS layer packed; the return is an owned encoding freed after the
    // copy into a JS Buffer.
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("napi_get_buffer_info(env, args[0], &contact_raw, &contact_len);"),
        "buffered param must read the packed Buffer: {addon}"
    );
    assert!(
        addon.contains(
            "const uint8_t* result = weaveffi_contacts_save((const uint8_t*)contact_raw, contact_len, &out_len, &err);"
        ),
        "the call must pass ptr+len and thread out_len: {addon}"
    );
    assert!(
        addon.contains("napi_create_buffer_copy(env, out_len, result, NULL, &ret);"),
        "the buffered return must be copied into a JS Buffer: {addon}"
    );
    assert!(
        addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
        "the owned encoding must be freed with weaveffi_free_bytes: {addon}"
    );
    // Records have no native helpers at all.
    assert!(
        !addon.contains("Contact_get_") && !addon.contains("Contact_destroy"),
        "records must not have native getters or destructors: {addon}"
    );

    // Loader: generated pack/unpack write fields in declaration order and
    // the wrapper encodes the argument and decodes the result.
    let index = index_for(&api, true);
    assert!(
        index.contains("function __packContact(w, v) {"),
        "missing pack function: {index}"
    );
    let name_write = index.find("w.str(v.name);").expect("pack writes name");
    let age_write = index.find("w.i32(v.age);").expect("pack writes age");
    assert!(
        name_write < age_write,
        "fields must pack in declaration order: {index}"
    );
    assert!(
        index.contains("function __unpackContact(r) {")
            && index.contains("name: r.str(),")
            && index.contains("age: r.i32(),"),
        "missing unpack function: {index}"
    );
    assert!(
        index.contains(
            "const _r = __invoke(addon.save, [__encode(__packContact, contact)], __generic);"
        ),
        "the wrapper must pack the record argument: {index}"
    );
    assert!(
        index.contains("return __decode(__unpackContact, _r);"),
        "the wrapper must decode the record result: {index}"
    );
}

#[test]
fn optional_record_return_is_buffered() {
    // `Contact?` is buffered (the absence flag lives inside the buffer),
    // so the addon must not null-check the pointer; the JS layer decodes
    // the flag byte instead.
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "find",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            false,
        ));
        m
    }]);
    let addon = addon_for(&api, true);
    assert!(
        !addon.contains("if (result == NULL)"),
        "buffered optional must not null-check the pointer: {addon}"
    );
    assert!(
        addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
        "buffered optional return must be freed: {addon}"
    );
    let index = index_for(&api, true);
    assert!(
        index.contains("return __decode((r) => __rOpt(r, __unpackContact), _r);"),
        "the wrapper must decode through the optional combinator: {index}"
    );
}

#[test]
fn async_buffered_result_copied_then_decoded() {
    let api = make_api(vec![{
        let mut m = make_module("tasks");
        m.structs.push(contact_struct());
        m.functions.push(Function {
            name: "fetch_contact".into(),
            params: vec![],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        });
        m
    }]);

    // The completion callback receives a BORROWED buffer: it must copy
    // the bytes before returning (the producer frees them afterwards).
    let addon = addon_for(&api, true);
    assert!(
        addon.contains(
            "static void weaveffi_tasks_fetch_contact_napi_cb(void* context, weaveffi_error* err, const uint8_t* result_ptr, size_t result_len) {"
        ),
        "callback must take the borrowed buffer slots: {addon}"
    );
    assert!(
        addon.contains(
            "ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result_ptr, result_len);"
        ),
        "callback must deep-copy the borrowed buffer: {addon}"
    );
    assert!(
        addon.contains("napi_create_buffer_copy(env, ctx->result_len,"),
        "settle must surface the copied bytes as a JS Buffer: {addon}"
    );

    // The JS wrapper decodes the resolved buffer.
    let index = index_for(&api, true);
    assert!(
        index.contains(
            "return __invokeAsync(addon.fetchContact, [], __generic).then((_r) => __decode(__unpackContact, _r));"
        ),
        "the async wrapper must decode the resolved buffer: {index}"
    );
}

#[test]
fn iterator_buffered_elements_decoded_and_freed() {
    let api = make_api(vec![{
        let mut m = make_module("contacts");
        m.structs.push(contact_struct());
        m.functions.push(func(
            "iter_contacts",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            false,
        ));
        m
    }]);

    // Addon: `_next` pulls the encoded element plus its length, copies it
    // into a JS Buffer, then releases it with weaveffi_free_bytes.
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("size_t iter_item_len = 0;"),
        "buffered elements need the extra length slot: {addon}"
    );
    assert!(
        addon.contains("&iter_item, &iter_item_len, &iter_err"),
        "next must thread the element length out-param: {addon}"
    );
    assert!(
        addon.contains("napi_create_buffer_copy(env, iter_item_len, iter_item, NULL, &ret);"),
        "the element must be copied into a JS Buffer: {addon}"
    );
    assert!(
        addon.contains("weaveffi_free_bytes((uint8_t*)iter_item, iter_item_len);"),
        "the element encoding must be freed after copying: {addon}"
    );

    // Loader: the lazy iterator decodes each element buffer per step.
    let index = index_for(&api, true);
    assert!(
        index.contains(
            "return new WeaveFFIIterator(_it, addon.iterContacts_iterNext, addon.iterContacts_iterDestroy, __generic, (_e) => __decode(__unpackContact, _e));"
        ),
        "the iterator wrapper must decode each element: {index}"
    );
}

#[test]
fn error_payload_fields_decoded_and_attached() {
    let api = make_api(vec![{
        let mut m = make_module("kv");
        m.functions.push(func(
            "get",
            vec![param("key", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
            true,
        ));
        m.errors = Some(ErrorDomain {
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
                    name: "StoreFull".into(),
                    code: 1003,
                    message: "store is full".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        });
        m
    }]);

    // Addon: the native error helper attaches the raw payload buffer.
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("napi_set_named_property(env, err, \"payload\", payload_val);"),
        "the error helper must attach the payload buffer: {addon}"
    );
    assert!(
        addon.contains(
            "napi_throw(env, weaveffi_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));"
        ),
        "the sync throw must pass the payload slots: {addon}"
    );

    // Loader: codes with fields get a payload decoder; the factory
    // attaches the decoded fields as properties on the error.
    let index = index_for(&api, true);
    assert!(
        index.contains(
            "const __kvErrorPayloads = Object.freeze({ 1001: (r) => ({ key: r.str(), attempts: r.i32() }) });"
        ),
        "missing the per-code payload decoders: {index}"
    );
    assert!(
        index.contains("function __kvErrorFrom(code, message, payload) {"),
        "the factory must accept the payload buffer: {index}"
    );
    assert!(
        index.contains("Object.assign(_err, __decode(_decode, payload));"),
        "decoded payload fields must land as error properties: {index}"
    );

    // Declarations: the payload fields surface as readonly properties on
    // the code's error class.
    let dts = dts_for(&api, true);
    assert!(
        dts.contains("export class KeyNotFoundError extends KvError {"),
        "missing per-code class: {dts}"
    );
    assert!(
        dts.contains("  readonly key: string;") && dts.contains("  readonly attempts: number;"),
        "payload fields must be declared on the class: {dts}"
    );
}

#[test]
fn listener_buffered_params_decoded() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![contact_struct()],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnContact".into(),
            doc: None,
            params: vec![param("contact", TypeRef::Record("Contact".into()))],
        }],
        listeners: vec![ListenerDef {
            name: "contact_listener".into(),
            event_callback: "OnContact".into(),
            doc: None,
        }],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    // Addon: the borrowed (ptr, len) argument is deep-copied by the
    // trampoline, surfaced as a JS Buffer by the marshaller, then freed.
    let addon = addon_for(&api, true);
    assert!(
        addon.contains("uint8_t* contact_ptr;") && addon.contains("size_t contact_len;"),
        "the payload struct must own the copied buffer: {addon}"
    );
    assert!(
        addon.contains(
            "if (contact_ptr != NULL && contact_len > 0) { p->contact_ptr = (uint8_t*)malloc(contact_len); memcpy(p->contact_ptr, contact_ptr, contact_len); }"
        ),
        "the trampoline must deep-copy the borrowed buffer: {addon}"
    );
    assert!(
        addon.contains("napi_create_buffer_copy(env, p->contact_len,"),
        "the marshaller must surface the copied buffer: {addon}"
    );
    assert!(
        addon.contains("free(p->contact_ptr);"),
        "the payload copy must be freed after the JS call: {addon}"
    );

    // Loader: the register wrapper decodes the buffer before invoking the
    // user's callback.
    let index = index_for(&api, true);
    assert!(
        index.contains("wv.registerContactListener = function (callback) {"),
        "missing the register wrapper: {index}"
    );
    assert!(
        index.contains("callback(__decode(__unpackContact, contact));"),
        "the wrapper must decode the buffered argument: {index}"
    );

    // Declarations type the callback in terms of the record.
    let dts = dts_for(&api, true);
    assert!(
        dts.contains(
            "export function registerContactListener(callback: (contact: Contact) => void): number"
        ),
        "register dts must type the record param: {dts}"
    );
}

// --- Rich (algebraic) enum support ------------------------------------

/// A module mirroring `samples/shapes/shapes.yml`: a rich enum `Shape`
/// (unit + f64 + two-f32 + string/u8 variants), a plain enum `Channel`, and
/// the free functions that take/return the rich enum plus a numeric smoke.
fn shapes_module() -> Module {
    fn variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
        EnumVariant {
            name: name.into(),
            value,
            doc: None,
            fields,
        }
    }
    Module {
        name: "shapes".into(),
        functions: vec![
            func(
                "describe",
                vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::StringUtf8),
                false,
            ),
            func(
                "scale",
                vec![
                    param("shape", TypeRef::RichEnum("Shape".into())),
                    param("factor", TypeRef::F64),
                ],
                Some(TypeRef::RichEnum("Shape".into())),
                false,
            ),
            func(
                "sum_bytes",
                vec![param("values", TypeRef::List(Box::new(TypeRef::U8)))],
                Some(TypeRef::U64),
                false,
            ),
        ],
        structs: vec![],
        enums: vec![
            EnumDef {
                name: "Shape".into(),
                doc: None,
                variants: vec![
                    variant("Empty", 0, vec![]),
                    variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                    variant(
                        "Rectangle",
                        2,
                        vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                    ),
                    variant(
                        "Labeled",
                        3,
                        vec![
                            field("label", TypeRef::StringUtf8),
                            field("count", TypeRef::U8),
                        ],
                    ),
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
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }
}

#[test]
fn rich_enum_addon_marshals_value_buffers() {
    let addon = addon_for(&make_api(vec![shapes_module()]), false);

    // Rich enums are value types: no tag reader, no per-variant
    // constructors or getters, no destructor.
    for gone in [
        "Shape_tag",
        "Shape_Empty_new",
        "Shape_Circle_new",
        "Shape_Circle_get_radius",
        "Shape_destroy",
    ] {
        assert!(
            !addon.contains(gone),
            "rich enums must have no native helper {gone}: {addon}"
        );
    }

    // Free functions marshal the rich enum as a value buffer, in and out.
    assert!(
        addon.contains("napi_get_buffer_info(env, args[0], &shape_raw, &shape_len);"),
        "describe must read the packed shape buffer: {addon}"
    );
    assert!(
        addon.contains("weaveffi_shapes_describe((const uint8_t*)shape_raw, shape_len, &err);"),
        "describe must pass the borrowed ptr+len pair: {addon}"
    );
    assert!(
        addon.contains(
            "const uint8_t* result = weaveffi_shapes_scale((const uint8_t*)shape_raw, shape_len, factor, &out_len, &err);"
        ),
        "scale must take a buffer and return an owned one: {addon}"
    );
    assert!(
        addon.contains("weaveffi_free_bytes((uint8_t*)result, out_len);"),
        "the returned encoding must be freed: {addon}"
    );

    // A list<u8> parameter is buffered too.
    assert!(
        addon.contains("weaveffi_shapes_sum_bytes((const uint8_t*)values_raw, values_len, &err);"),
        "list params must cross as value buffers: {addon}"
    );
}

#[test]
fn rich_enum_index_js_packs_tagged_unions() {
    let index = index_for(&make_api(vec![shapes_module()]), false);

    // Pack: the string tag selects the variant, then the i32 discriminant
    // plus the variant's fields go on the wire in order.
    assert!(
        index.contains("function __packShape(w, v) {"),
        "missing pack function: {index}"
    );
    assert!(
        index.contains("case 'Circle':") && index.contains("w.i32(1);"),
        "circle variant must pack its discriminant: {index}"
    );
    assert!(
        index.contains("w.f64(v.radius);"),
        "circle variant must pack its field: {index}"
    );
    assert!(
        index.contains("w.f32(v.width);") && index.contains("w.f32(v.height);"),
        "rectangle variant must pack both f32 fields: {index}"
    );
    assert!(
        index.contains("w.str(v.label);") && index.contains("w.u8(v.count);"),
        "labeled variant must pack string + u8: {index}"
    );
    // An unknown tag is a caller bug surfaced as the generic brand.
    assert!(
        index.contains("throw new WeaveFFIError(-2, 'unknown Shape tag: ' + (v && v.tag));"),
        "pack must reject unknown tags: {index}"
    );

    // Unpack: the i32 discriminant selects the variant; fields land next
    // to the string tag.
    assert!(
        index.contains("function __unpackShape(r) {"),
        "missing unpack function: {index}"
    );
    assert!(
        index.contains("case 0: return { tag: 'Empty' };"),
        "unit variant must unpack to a bare tag: {index}"
    );
    assert!(
        index.contains("case 1: return { tag: 'Circle', radius: r.f64() };"),
        "circle variant must unpack its field: {index}"
    );
    assert!(
        index.contains("case 3: return { tag: 'Labeled', label: r.str(), count: r.u8() };"),
        "labeled variant must unpack in field order: {index}"
    );
    assert!(
        index.contains("default: throw new WeaveFFIError(-2, 'unknown Shape tag: ' + tag);"),
        "unpack must reject unknown discriminants: {index}"
    );

    // Wrappers pack arguments and decode results; no classes, no handles.
    assert!(
        index.contains("wv.shapesScale = function (shape, factor) {")
            && index.contains(
                "const _r = __invoke(addon.shapesScale, [__encode(__packShape, shape), factor], __generic);"
            )
            && index.contains("return __decode(__unpackShape, _r);"),
        "scale must pack its argument and decode its result: {index}"
    );
    assert!(
        index.contains(
            "return __invoke(addon.shapesDescribe, [__encode(__packShape, shape)], __generic);"
        ),
        "describe must pack its argument: {index}"
    );
    assert!(
        !index.contains("class Shape"),
        "rich enums must not surface as classes: {index}"
    );
}

#[test]
fn index_js_without_domains_wraps_with_generic_brand() {
    // Even with no rich enums, interfaces, or error domains, every
    // function gets a wrapper so a non-zero error slot (panic or
    // marshalling failure) surfaces as the generic brand class.
    let mut m = make_module("math");
    m.functions.push(func(
        "add",
        vec![param("a", TypeRef::I32)],
        Some(TypeRef::I32),
        false,
    ));
    let index = index_for(&make_api(vec![m]), false);
    assert!(
        index.contains("class WeaveFFIError extends Error {"),
        "generic brand class missing: {index}"
    );
    assert!(
        index.contains("wv.mathAdd = function (a) {")
            && index.contains("return __invoke(addon.mathAdd, [a], __generic);"),
        "non-throwing fn must wrap through the generic brand: {index}"
    );
    assert!(
        index.contains("module.exports = wv;"),
        "index must export the wrapper namespace: {index}"
    );
}

#[test]
fn rich_enum_dts_emits_tagged_union() {
    let dts = dts_for(&make_api(vec![shapes_module()]), false);

    // Rich enum -> a discriminated union keyed by a string tag.
    assert!(
        dts.contains("export type Shape ="),
        "rich enum must be a union type: {dts}"
    );
    assert!(
        !dts.contains("export enum Shape") && !dts.contains("export class Shape"),
        "rich enum must not be a plain enum or a class: {dts}"
    );
    assert!(dts.contains("| { tag: 'Empty' }"), "{dts}");
    assert!(dts.contains("| { tag: 'Circle'; radius: number }"), "{dts}");
    assert!(
        dts.contains("| { tag: 'Rectangle'; width: number; height: number }"),
        "{dts}"
    );
    assert!(
        dts.contains("| { tag: 'Labeled'; label: string; count: number }"),
        "{dts}"
    );

    // Plain enum still surfaces as a numeric `enum`.
    assert!(
        dts.contains("export enum Channel {"),
        "plain enum stays an enum: {dts}"
    );

    // Free functions are typed in terms of the union; unstripped names
    // keep the module prefix but are still lowerCamelCase.
    assert!(
        dts.contains("export function shapesDescribe(shape: Shape): string"),
        "{dts}"
    );
    assert!(
        dts.contains("export function shapesScale(shape: Shape, factor: number): Shape"),
        "{dts}"
    );
}

// --- Interfaces and typed errors ----------------------------------------

/// A module mirroring the kvstore sample's shape: a `KvError` domain, a
/// `Store` interface (canonical `new` + non-throwing factory + throwing
/// and non-throwing methods + an async method + a static), and free
/// functions exercising the throws split and interface params/returns.
fn kv_module() -> Module {
    Module {
        name: "kv".into(),
        functions: vec![
            func("ping", vec![], Some(TypeRef::Bool), false),
            func(
                "clone_store",
                vec![param("source_store", TypeRef::Interface("Store".into()))],
                Some(TypeRef::Interface("Store".into())),
                true,
            ),
        ],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store.".into()),
            constructors: vec![
                func("new", vec![param("path", TypeRef::StringUtf8)], None, true),
                func(
                    "open_readonly",
                    vec![param("path", TypeRef::StringUtf8)],
                    None,
                    false,
                ),
            ],
            methods: vec![
                func(
                    "put",
                    vec![
                        param("key", TypeRef::StringUtf8),
                        param("the_value", TypeRef::StringUtf8),
                    ],
                    None,
                    true,
                ),
                func("count", vec![], Some(TypeRef::I64), false),
                func(
                    "list_keys",
                    vec![param(
                        "prefix",
                        TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    )],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                    true,
                ),
                Function {
                    name: "compact".into(),
                    params: vec![],
                    returns: Some(TypeRef::I64),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    throws: true,
                    deprecated: None,
                    since: None,
                },
            ],
            statics: vec![func("default_capacity", vec![], Some(TypeRef::I64), false)],
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: Some("The requested key does not exist.".into()),
                    fields: vec![],
                },
                ErrorCode {
                    name: "StoreFull".into(),
                    code: 1003,
                    message: "store is full".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }
}

#[test]
fn interface_addon_exposes_member_entry_points() {
    let addon = addon_for(&make_api(vec![kv_module()]), true);

    // One native entry point per member plus the destructor, all named
    // from the model's `{c_tag}_{member}` symbols.
    for sym in [
        "static napi_value Napi_weaveffi_kv_Store_new(",
        "static napi_value Napi_weaveffi_kv_Store_open_readonly(",
        "static napi_value Napi_weaveffi_kv_Store_put(",
        "static napi_value Napi_weaveffi_kv_Store_count(",
        "static napi_value Napi_weaveffi_kv_Store_compact(",
        "static napi_value Napi_weaveffi_kv_Store_default_capacity(",
        "static napi_value Napi_weaveffi_kv_Store_destroy(",
    ] {
        assert!(addon.contains(sym), "missing entry point {sym}: {addon}");
    }

    // Constructors return the owned object pointer as an int64 handle.
    assert!(
        addon.contains("weaveffi_kv_Store* result = weaveffi_kv_Store_new(path, &err);"),
        "ctor must call the C constructor: {addon}"
    );
    // Methods read the wrapped pointer from args[0] and pass it as the
    // leading C argument, ahead of the logical parameters.
    assert!(
        addon.contains(
            "weaveffi_kv_Store_put((const weaveffi_kv_Store*)(intptr_t)self_raw, key, the_value, &err);"
        ),
        "method must pass self first: {addon}"
    );
    // The async launcher symbol comes from the model (member base plus
    // `_async`), with the self slot leading.
    assert!(
        addon.contains("weaveffi_kv_Store_compact_async((const weaveffi_kv_Store*)(intptr_t)self_raw, weaveffi_kv_Store_compact_napi_cb, ctx);"),
        "async method must call the model's launcher with self: {addon}"
    );
    // The destructor frees the object.
    assert!(
        addon.contains("weaveffi_kv_Store_destroy(self);"),
        "destroy must free the object: {addon}"
    );

    // Members export under stripped, interface-scoped JS names.
    for js in [
        "\"Store_new\"",
        "\"Store_open_readonly\"",
        "\"Store_put\"",
        "\"Store_default_capacity\"",
        "\"Store_destroy\"",
    ] {
        assert!(addon.contains(js), "missing JS export {js}: {addon}");
    }

    // Every failure path throws the code-and-payload-carrying error object.
    assert!(
        addon.contains(
            "napi_throw(env, weaveffi_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));"
        ),
        "sync errors must carry the ABI code and payload: {addon}"
    );
    assert!(
        addon.contains("napi_set_named_property(env, err, \"code\", code_val);"),
        "the error helper must attach the numeric code: {addon}"
    );
}

#[test]
fn iterator_addon_is_lazy() {
    let addon = addon_for(&make_api(vec![kv_module()]), true);

    // The launch entry point never drains: it boxes the owned handle
    // into a state cell and wraps it in an external with a finalizer.
    assert!(
        !addon.contains("while (weaveffi_kv_Store_ListKeysIterator_next"),
        "the addon must not drain the iterator into an array: {addon}"
    );
    assert!(
        addon.contains(
            "weaveffi_napi_iter_state* iter_state = (weaveffi_napi_iter_state*)calloc(1, sizeof(weaveffi_napi_iter_state));"
        ),
        "launch must box the handle into a state cell: {addon}"
    );
    assert!(
        addon.contains(
            "napi_create_external(env, iter_state, weaveffi_kv_Store_list_keys_napi_iter_finalize, NULL, &ret);"
        ),
        "launch must wrap the cell in an external with a finalizer: {addon}"
    );

    // Per-iterator `next` and `destroy` entry points hang off the model's
    // iterator-tag symbols and export under the wrapper's addon name.
    assert!(
        addon.contains(
            "static napi_value Napi_weaveffi_kv_Store_ListKeysIterator_next(napi_env env, napi_callback_info info) {"
        ),
        "missing the per-iterator next entry point: {addon}"
    );
    assert!(
        addon.contains(
            "static napi_value Napi_weaveffi_kv_Store_ListKeysIterator_destroy(napi_env env, napi_callback_info info) {"
        ),
        "missing the per-iterator destroy entry point: {addon}"
    );
    assert!(
        addon.contains("\"Store_list_keys_iterNext\"")
            && addon.contains("\"Store_list_keys_iterDestroy\""),
        "next/destroy must export under the wrapper's addon names: {addon}"
    );

    // One producer pull per call, threading the per-step error slot.
    assert!(
        addon.contains(
            "if (!weaveffi_kv_Store_ListKeysIterator_next((weaveffi_kv_Store_ListKeysIterator*)state->iter, &iter_item, &iter_err)) {"
        ),
        "next must issue exactly one producer pull with the error slot: {addon}"
    );
    // A per-step fault throws the code-carrying error (list_keys is
    // `throws`, so the JS layer maps it to the domain class).
    assert!(
        addon.contains(
            "napi_throw(env, weaveffi_napi_error_value(env, iter_err.code, iter_err.message, iter_err.payload_ptr, iter_err.payload_len));"
        ),
        "next must throw the per-step error: {addon}"
    );
    // Each yielded string element is freed after the JS string exists.
    let convert = addon
        .find("napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);")
        .expect("next must convert the yielded element");
    let free = addon
        .find("weaveffi_free_string((char*)iter_item);")
        .expect("next must free the yielded string");
    assert!(
        convert < free,
        "the element must be converted before it is freed: {addon}"
    );

    // Every destroy site nulls the cell first, so exhaustion, explicit
    // destroy, and the finalizer never double-free.
    assert!(
        addon.contains(
            "weaveffi_kv_Store_ListKeysIterator_destroy((weaveffi_kv_Store_ListKeysIterator*)state->iter);"
        ),
        "destroy must release through the state cell: {addon}"
    );
    assert!(
        addon.contains("if (state != NULL && state->iter != NULL) {"),
        "explicit destroy must guard against double-destroy: {addon}"
    );
    assert!(
        addon.contains(
            "static void weaveffi_kv_Store_list_keys_napi_iter_finalize(napi_env env, void* data, void* hint) {"
        ),
        "abandoned iterators must be reclaimed by a finalizer: {addon}"
    );
}

#[test]
fn iterator_js_class_implements_protocol() {
    let index = index_for(&make_api(vec![kv_module()]), true);

    // The shared class implements the iterator protocol lazily.
    assert!(
        index.contains("class WeaveFFIIterator {"),
        "missing the shared iterator class: {index}"
    );
    assert!(
        index.contains("[Symbol.iterator]() {"),
        "the class must be iterable: {index}"
    );
    assert!(
        index.contains("return(value) {"),
        "the class must clean up on early exit: {index}"
    );
    // One native pull per step, routed through the rebranding helper.
    assert!(
        index.contains("const _v = __invoke(this._nextFn, [this._ext], this._map);"),
        "next() must issue one native pull: {index}"
    );
    // Early exit destroys the native handle exactly once.
    assert!(
        index.contains("this._destroyFn(this._ext);"),
        "return() must destroy the native handle: {index}"
    );

    // The method wrapper launches (packing the optional prefix into a
    // value buffer), then hands the external to the class with its
    // per-iterator next/destroy bindings and error mapping.
    assert!(
        index.contains(
            "const _it = __invoke(addon.Store_list_keys, [this._handle, __encode((w, v) => __wOpt(w, v, (w, v) => w.str(v)), prefix)], __kvErrorFrom);"
        ),
        "the wrapper must pack the optional param and launch: {index}"
    );
    assert!(
        index.contains(
            "return new WeaveFFIIterator(_it, addon.Store_list_keys_iterNext, addon.Store_list_keys_iterDestroy, __kvErrorFrom, null);"
        ),
        "the wrapper must return the lazy iterator: {index}"
    );
}

#[test]
fn iterator_dts_is_iterable_iterator() {
    let dts = dts_for(&make_api(vec![kv_module()]), true);
    assert!(
        dts.contains("IterableIterator<string>"),
        "iter<string> must surface as IterableIterator<string>: {dts}"
    );
    assert!(
        !dts.contains("string[]"),
        "iter<T> must not surface as an array: {dts}"
    );
}

#[test]
fn interface_index_js_class() {
    let index = index_for(&make_api(vec![kv_module()]), true);

    assert!(
        index.contains("class Store {"),
        "missing Store class: {index}"
    );
    // The canonical `new` constructor maps to the JS constructor and
    // routes failures through the domain factory (it throws).
    assert!(
        index.contains("constructor(path) {")
            && index.contains("this._handle = __invoke(addon.Store_new, [path], __kvErrorFrom);"),
        "missing canonical constructor: {index}"
    );
    // Other constructors become static factories; this one does not
    // throw, so failures rebrand as the generic class.
    assert!(
        index.contains("static openReadonly(path) {")
            && index.contains("__invoke(addon.Store_open_readonly, [path], __generic)")
            && index.contains("return Store._fromHandle(_r);"),
        "missing factory wrapping the owned handle: {index}"
    );
    // Methods pass the wrapped handle as the leading argument.
    assert!(
        index.contains("put(key, theValue) {")
            && index.contains(
                "return __invoke(addon.Store_put, [this._handle, key, theValue], __kvErrorFrom);"
            ),
        "missing method with leading self handle: {index}"
    );
    // The async method rejects typed (it throws).
    assert!(
        index.contains("compact() {")
            && index.contains(
                "return __invokeAsync(addon.Store_compact, [this._handle], __kvErrorFrom);"
            ),
        "missing async method: {index}"
    );
    // Statics are static methods.
    assert!(
        index.contains("static defaultCapacity() {")
            && index.contains("return __invoke(addon.Store_default_capacity, [], __generic);"),
        "missing static method: {index}"
    );
    // Disposal follows the opaque-wrapper idiom: explicit destroy plus a
    // FinalizationRegistry safety net calling the destroy export.
    assert!(
        index.contains("destroy() {") && index.contains("addon.Store_destroy(this._handle);"),
        "missing destroy(): {index}"
    );
    assert!(
        index.contains("Store._cleanup = new FinalizationRegistry"),
        "missing FinalizationRegistry: {index}"
    );

    // A free function borrowing an interface unwraps the class argument
    // and wraps the owned returned handle in a new instance.
    assert!(
        index.contains("wv.cloneStore = function (sourceStore) {")
            && index.contains(
                "__invoke(addon.cloneStore, [sourceStore instanceof Store ? sourceStore._handle : sourceStore], __kvErrorFrom)"
            )
            && index.contains("return Store._fromHandle(_r);"),
        "interface param/return must cross as instances: {index}"
    );
}

#[test]
fn typed_error_classes_js() {
    let index = index_for(&make_api(vec![kv_module()]), true);

    // Domain class extends the generic brand; per-code subclasses carry
    // their stable CODE and default message.
    assert!(
        index.contains("class KvError extends WeaveFFIError {"),
        "missing domain class: {index}"
    );
    assert!(
        index.contains("class KeyNotFoundError extends KvError {"),
        "missing per-code class: {index}"
    );
    assert!(
        index.contains("KeyNotFoundError.CODE = 1001;")
            && index.contains("StoreFullError.CODE = 1003;"),
        "missing stable code constants: {index}"
    );
    assert!(
        index.contains("super(1001, message || 'key not found');"),
        "per-code class must default its message: {index}"
    );
    // The factory maps a raw code (plus the raw payload buffer) to the
    // matching class and falls back to the generic brand for unknown
    // codes (panics, marshalling).
    assert!(
        index.contains("function __kvErrorFrom(code, message, payload) {"),
        "missing domain factory: {index}"
    );
    assert!(
        index.contains("1001: KeyNotFoundError, 1003: StoreFullError"),
        "missing code table: {index}"
    );
    assert!(
        index.contains(
            "const _err = _cls === undefined ? new WeaveFFIError(code, message) : new _cls(message);"
        ),
        "factory must fall back to the generic brand: {index}"
    );
    // Both surfaces are exported.
    assert!(
        index.contains("wv.KvError = KvError;")
            && index.contains("wv.KeyNotFoundError = KeyNotFoundError;"),
        "error classes must be exported: {index}"
    );
}

#[test]
fn throws_split_picks_the_error_surface() {
    let index = index_for(&make_api(vec![kv_module()]), true);

    // throws == false: plain wrapper; a non-zero error slot (panic or
    // marshalling failure only) still rebrands as the generic class.
    assert!(
        index.contains("wv.ping = function () {")
            && index.contains("return __invoke(addon.ping, [], __generic);"),
        "non-throwing fn must use the generic map: {index}"
    );
    // throws == true: failures map through the module's domain factory.
    assert!(
        index.contains("__invoke(addon.cloneStore, [sourceStore instanceof Store ? sourceStore._handle : sourceStore], __kvErrorFrom)"),
        "throwing fn must use the domain map: {index}"
    );
}

#[test]
fn typed_error_and_interface_dts() {
    let dts = dts_for(&make_api(vec![kv_module()]), true);

    // The generic brand plus the domain surface.
    assert!(
        dts.contains("export class WeaveFFIError extends Error {"),
        "missing generic brand: {dts}"
    );
    assert!(
        dts.contains("export class KvError extends WeaveFFIError {"),
        "missing domain class: {dts}"
    );
    assert!(
        dts.contains("export class KeyNotFoundError extends KvError {")
            && dts.contains("static readonly CODE: 1001;"),
        "missing per-code class: {dts}"
    );

    // The interface class mirrors the JS surface.
    assert!(
        dts.contains("export class Store {"),
        "missing Store class: {dts}"
    );
    assert!(
        dts.contains("constructor(path: string);"),
        "missing canonical constructor: {dts}"
    );
    assert!(
        dts.contains("static openReadonly(path: string): Store;"),
        "missing factory: {dts}"
    );
    assert!(
        dts.contains("put(key: string, theValue: string): void;"),
        "missing method with camel params: {dts}"
    );
    assert!(
        dts.contains("compact(): Promise<number>;"),
        "missing async method: {dts}"
    );
    assert!(
        dts.contains("static defaultCapacity(): number;"),
        "missing static: {dts}"
    );
    assert!(dts.contains("destroy(): void;"), "missing destroy: {dts}");

    // Throwing callables document their domain; interface params and
    // returns are typed as the class.
    assert!(
        dts.contains("@throws {KvError}"),
        "missing @throws tag: {dts}"
    );
    assert!(
        dts.contains("export function cloneStore(sourceStore: Store): Store"),
        "missing interface-typed free function: {dts}"
    );
    assert!(
        dts.contains("export function ping(): boolean"),
        "missing plain function: {dts}"
    );
}
