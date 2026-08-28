//! Output-content tests for the .NET generator: fixtures mirroring the
//! sample IDLs plus assertions over the rendered C# source and manifests.

use weaveffi_ir::ir::Api;
use weaveffi_core::resolved::ResolvedApi;
use super::*;
use crate::types::{cs_type, pinvoke_type, safe_cs_name};
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

/// Test shim matching the pre-0.5.0 signature: builds the [`BindingModel`]
/// here so the production `render_csharp` stays model-only.
fn render_csharp(
    api: &ResolvedApi,
    namespace: &str,
    strip_module_prefix: bool,
    prefix: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let model = BindingModel::build(api, prefix);
    super::render_csharp(
        &model,
        namespace,
        strip_module_prefix,
        input_basename,
        filename,
    )
}

#[test]
fn package_emits_runtimes_and_rebinds_libname() {
    use camino::Utf8Path;
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = make_api(vec![simple_module(vec![Function {
        name: "ping".into(),
        params: vec![],
        returns: None,
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &DotnetGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &DotnetConfig::default(),
    )
    .expect("dotnet supports packaging");

    // NuGet `runtimes/<rid>/native/` layout.
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("runtimes/osx-arm64/native/libcalculator.dylib")));
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("runtimes/win-x64/native/calculator.dll")));
    // The P/Invoke library name is rebound to the bundled base name.
    let cs = files
        .iter()
        .find(|f| f.path.as_str().ends_with(".cs"))
        .expect("C# source present");
    let FileContent::Text(src) = &cs.content else {
        panic!("C# source is text");
    };
    assert!(
        src.contains("private const string LibName = \"calculator\";"),
        "DllImport name not rebound: {src}"
    );
    let csproj = files
        .iter()
        .find(|f| f.path.as_str().ends_with(".csproj"))
        .expect("csproj present");
    let FileContent::Text(proj) = &csproj.content else {
        panic!("csproj is text");
    };
    assert!(
        proj.contains("runtimes/**"),
        "native asset item group missing: {proj}"
    );
}

fn make_api(modules: Vec<Module>) -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules,
        generators: None,
        package: None,
    })
}

fn simple_module(functions: Vec<Function>) -> Module {
    Module {
        name: "math".into(),
        functions,
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }
}

#[test]
fn generator_name_is_dotnet() {
    assert_eq!(Generator::name(&DotnetGenerator), "dotnet");
}

#[test]
fn output_files_lists_all() {
    let api = make_api(vec![]);
    let out = Utf8Path::new("/tmp/out");
    let files = DotnetGenerator.output_files(&api, out, &DotnetConfig::default());
    assert_eq!(
        files,
        vec![
            format!("{out}/dotnet/README.md"),
            format!("{out}/dotnet/WeaveFFI.cs"),
            format!("{out}/dotnet/WeaveFFI.csproj"),
            format!("{out}/dotnet/WeaveFFI.nuspec"),
        ]
    );
}

#[test]
fn generate_creates_output_file() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_gen_output");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator
        .generate(&api, out_dir, &DotnetConfig::default())
        .unwrap();

    let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
    assert!(cs.contains("namespace WeaveFFI"));
    assert!(cs.contains("DllImport"));
    assert!(cs.contains("weaveffi_math_add"));

    let _ = std::fs::remove_dir_all(&tmp);
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
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    DotnetGenerator
        .generate(&api, out, &DotnetConfig::default())
        .unwrap();
    let cs = std::fs::read_to_string(dir.path().join("dotnet/WeaveFFI.cs")).unwrap();
    assert!(
        cs.contains("internal delegate void Cb_weaveffi_events_OnMessage_fn"),
        "unmanaged delegate type must be declared: {cs}"
    );
    assert!(
        cs.contains("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]"),
        "delegate must use cdecl: {cs}"
    );
    assert!(
        cs.contains("internal static extern ulong weaveffi_events_register_message_listener"),
        "register pinvoke missing: {cs}"
    );
    assert!(
        cs.contains("public static ulong RegisterMessageListener(Action<string> callback)"),
        "register wrapper missing: {cs}"
    );
    assert!(
        cs.contains("public static void UnregisterMessageListener(ulong id)"),
        "unregister wrapper missing: {cs}"
    );
    assert!(
        cs.contains("_listenerRefs[id] = trampoline;"),
        "delegate must be pinned in the registry: {cs}"
    );
    assert!(
        cs.contains("Marshal.PtrToStringUTF8(message) ?? \"\""),
        "string arg must be marshaled: {cs}"
    );
}

#[test]
fn dotnet_record_is_plain_value_class() {
    let api = ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules: vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }],
        generators: None,
        package: None,
    });
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    DotnetGenerator
        .generate(&api, out, &DotnetConfig::default())
        .unwrap();
    let dotnet_dir = out.join("dotnet");
    let cs_files: Vec<_> = std::fs::read_dir(&dotnet_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "cs").unwrap_or(false))
        .collect();
    assert!(!cs_files.is_empty(), "expected .cs files");
    let cs = std::fs::read_to_string(cs_files[0].path()).unwrap();
    // A record is a plain sealed data class with typed get-only
    // properties and a positional constructor; builders are gone.
    assert!(
        cs.contains("public sealed class Contact"),
        "missing record class: {cs}"
    );
    assert!(
        cs.contains("public string Name { get; }") && cs.contains("public int Age { get; }"),
        "missing typed properties: {cs}"
    );
    assert!(
        cs.contains("public Contact(string name, int age)"),
        "missing positional constructor: {cs}"
    );
    // The value-buffer pack/unpack pair replaces every C symbol.
    assert!(
        cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
            && cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"),
        "missing pack/unpack pair: {cs}"
    );
    assert!(
        cs.contains("writer.WriteString(Name);") && cs.contains("writer.WriteI32(Age);"),
        "missing field encoding: {cs}"
    );
    assert!(
        !cs.contains("ContactBuilder") && !cs.contains("weaveffi_contacts_Contact_create"),
        "builder machinery must be gone: {cs}"
    );
    assert!(
        !cs.contains("class Contact : IDisposable"),
        "records must not be disposable: {cs}"
    );
}

#[test]
fn dotnet_generates_csproj() {
    let api = make_api(vec![simple_module(vec![])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_csproj");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator
        .generate(&api, out_dir, &DotnetConfig::default())
        .unwrap();

    let csproj_path = tmp.join("dotnet/WeaveFFI.csproj");
    assert!(csproj_path.exists(), ".csproj file must exist");
    let csproj = std::fs::read_to_string(&csproj_path).unwrap();
    assert!(
        csproj.contains(r#"Sdk="Microsoft.NET.Sdk""#),
        "missing SDK attribute: {csproj}"
    );
    assert!(
        csproj.contains("<TargetFramework>net8.0</TargetFramework>"),
        "missing target framework: {csproj}"
    );
    assert!(
        csproj.contains("<PackageId>WeaveFFI</PackageId>"),
        "missing package id: {csproj}"
    );
    assert!(
        csproj.contains("<Version>0.1.0</Version>"),
        "missing version: {csproj}"
    );

    let nuspec_path = tmp.join("dotnet/WeaveFFI.nuspec");
    assert!(nuspec_path.exists(), ".nuspec file must exist");
    let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
    assert!(
        nuspec.contains("<id>WeaveFFI</id>"),
        "missing nuspec id: {nuspec}"
    );

    let readme_path = tmp.join("dotnet/README.md");
    assert!(readme_path.exists(), "README.md must exist");
    let readme = std::fs::read_to_string(&readme_path).unwrap();
    assert!(
        readme.contains("dotnet build"),
        "missing build instructions: {readme}"
    );
    assert!(
        readme.contains("dotnet pack"),
        "missing pack instructions: {readme}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn cs_type_mapping() {
    assert_eq!(cs_type(&TypeRef::I32), "int");
    assert_eq!(cs_type(&TypeRef::U32), "uint");
    assert_eq!(cs_type(&TypeRef::I64), "long");
    assert_eq!(cs_type(&TypeRef::F64), "double");
    assert_eq!(cs_type(&TypeRef::Bool), "bool");
    assert_eq!(cs_type(&TypeRef::StringUtf8), "string");
    assert_eq!(cs_type(&TypeRef::Handle), "ulong");
    assert_eq!(cs_type(&TypeRef::Bytes), "byte[]");
    assert_eq!(cs_type(&TypeRef::Record("Foo".into())), "Foo");
    assert_eq!(cs_type(&TypeRef::Enum("Bar".into())), "Bar");
    assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        "string?"
    );
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("X".into())))),
        "X?"
    );
    assert_eq!(cs_type(&TypeRef::List(Box::new(TypeRef::I32))), "int[]");
    assert_eq!(
        cs_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "Dictionary<string, int>"
    );
}

#[test]
fn pinvoke_type_mapping() {
    assert_eq!(pinvoke_type(&TypeRef::I32), "int");
    // C `bool` is one byte, not int-widened.
    assert_eq!(pinvoke_type(&TypeRef::Bool), "byte");
    assert_eq!(pinvoke_type(&TypeRef::RichEnum("Foo".into())), "IntPtr");
    assert_eq!(pinvoke_type(&TypeRef::StringUtf8), "IntPtr");
    assert_eq!(pinvoke_type(&TypeRef::Handle), "ulong");
    assert_eq!(pinvoke_type(&TypeRef::Bytes), "IntPtr");
    assert_eq!(pinvoke_type(&TypeRef::Record("Foo".into())), "IntPtr");
    assert_eq!(pinvoke_type(&TypeRef::Enum("Bar".into())), "int");
    assert_eq!(
        pinvoke_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "IntPtr"
    );
}

#[test]
fn simple_i32_function() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(cs.contains("namespace WeaveFFI"), "missing namespace: {cs}");
    assert!(cs.contains("DllImport"), "missing DllImport: {cs}");
    assert!(cs.contains("weaveffi_math_add"), "missing C symbol: {cs}");
    assert!(
        cs.contains("CallingConvention.Cdecl"),
        "missing Cdecl: {cs}"
    );
    assert!(
        cs.contains("public static int Add("),
        "missing wrapper method: {cs}"
    );
    assert!(
        cs.contains("WeaveFFIError.Check(err)"),
        "missing error check: {cs}"
    );
}

#[test]
fn void_function() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "reset".into(),
        params: vec![],
        returns: None,
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static void Reset()"),
        "missing void wrapper: {cs}"
    );
    assert!(
        cs.contains("static extern void weaveffi_math_reset"),
        "missing void P/Invoke: {cs}"
    );
}

#[test]
fn bool_uses_int_marshaling() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "is_valid".into(),
        params: vec![Param {
            name: "flag".into(),
            ty: TypeRef::Bool,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::Bool),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("flag ? 1 : 0"),
        "missing bool-to-int conversion: {cs}"
    );
    assert!(
        cs.contains("result != 0"),
        "missing int-to-bool conversion: {cs}"
    );
}

#[test]
fn enum_generation() {
    let api = make_api(vec![Module {
        name: "paint".into(),
        functions: vec![Function {
            name: "mix".into(),
            params: vec![Param {
                name: "a".into(),
                ty: TypeRef::Enum("Color".into()),
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Enum("Color".into())),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
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
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(cs.contains("public enum Color"), "missing enum: {cs}");
    assert!(cs.contains("Red = 0"), "missing Red: {cs}");
    assert!(cs.contains("Green = 1"), "missing Green: {cs}");
    assert!(cs.contains("Blue = 2"), "missing Blue: {cs}");
    assert!(
        cs.contains("<summary>Primary colors</summary>"),
        "missing doc: {cs}"
    );
    assert!(cs.contains("(int)a"), "missing enum-to-int cast: {cs}");
    assert!(
        cs.contains("(Color)result"),
        "missing int-to-enum cast: {cs}"
    );
}

#[test]
fn struct_is_sealed_value_class_with_doc() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: Some("A contact record".into()),
            fields: vec![
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public sealed class Contact"),
        "missing sealed value class: {cs}"
    );
    assert!(
        cs.contains("public Contact(string firstName, int age)"),
        "missing positional constructor: {cs}"
    );
    assert!(
        cs.contains("<summary>A contact record</summary>"),
        "missing doc: {cs}"
    );
    // Records hold no native resources: no handle, no IDisposable.
    assert!(
        !cs.contains("Contact : IDisposable") && !cs.contains("internal Contact(IntPtr"),
        "record must not wrap a handle: {cs}"
    );
}

#[test]
fn struct_has_property_getters() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                },
                StructField {
                    name: "role".into(),
                    ty: TypeRef::Enum("Role".into()),
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public string FirstName { get; }"),
        "missing FirstName property: {cs}"
    );
    assert!(
        cs.contains("public int Age { get; }"),
        "missing Age property: {cs}"
    );
    assert!(
        cs.contains("public bool Active { get; }"),
        "missing Active property: {cs}"
    );
    assert!(
        cs.contains("public Role Role { get; }"),
        "missing Role property: {cs}"
    );
    // WriteTo serializes each field per the wire format; ReadFrom is the
    // exact inverse. No getter symbols cross the ABI anymore.
    assert!(
        cs.contains("writer.WriteString(FirstName);")
            && cs.contains("writer.WriteI32(Age);")
            && cs.contains("writer.WriteBool(Active);")
            && cs.contains("writer.WriteI32((int)Role);"),
        "missing field encodings: {cs}"
    );
    assert!(
        cs.contains("var fFirstName = reader.ReadString();")
            && cs.contains("var fAge = reader.ReadI32();")
            && cs.contains("var fActive = reader.ReadBool();")
            && cs.contains("var fRole = (Role)reader.ReadI32();"),
        "missing field decodings: {cs}"
    );
    assert!(
        !cs.contains("weaveffi_contacts_Contact_get_first_name"),
        "getter symbols must be gone: {cs}"
    );
}

#[test]
fn struct_has_no_dispose_or_finalizer() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I32,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // A record owns no native memory, so nothing to dispose or finalize.
    assert!(
        cs.contains("public sealed class Contact"),
        "missing record class: {cs}"
    );
    assert!(
        !cs.contains("weaveffi_contacts_Contact_destroy"),
        "destroy symbol must be gone: {cs}"
    );
    assert!(!cs.contains("~Contact()"), "finalizer must be gone: {cs}");
}

#[test]
fn struct_emits_no_pinvoke_declarations() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Records cross the ABI only inside value buffers, so no per-record
    // P/Invoke declarations exist. The shared runtime imports remain.
    assert!(
        !cs.contains("weaveffi_contacts_Contact_"),
        "record symbols must be gone: {cs}"
    );
    assert!(
        cs.contains("internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);"),
        "missing free_bytes runtime import: {cs}"
    );
    assert!(
        cs.contains("internal static extern void weaveffi_error_clear(ref WeaveFFIError err);"),
        "missing error_clear runtime import: {cs}"
    );
}

#[test]
fn string_function_uses_utf8() {
    let api = make_api(vec![Module {
        name: "text".into(),
        functions: vec![Function {
            name: "echo".into(),
            params: vec![Param {
                name: "msg".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("Marshal.PtrToStringUTF8(result)"),
        "missing PtrToStringUTF8: {cs}"
    );
    assert!(
        cs.contains("Marshal.StringToCoTaskMemUTF8(msg)"),
        "missing StringToCoTaskMemUTF8: {cs}"
    );
    assert!(
        cs.contains("Marshal.FreeCoTaskMem(msgPtr)"),
        "missing FreeCoTaskMem: {cs}"
    );
    assert!(
        cs.contains("weaveffi_free_string(result)"),
        "missing free_string: {cs}"
    );
}

#[test]
fn safe_cs_name_escapes_keywords() {
    assert_eq!(safe_cs_name("string"), "@string");
    assert_eq!(safe_cs_name("class"), "@class");
    assert_eq!(safe_cs_name("return"), "@return");
    assert_eq!(safe_cs_name("name"), "name");
    assert_eq!(safe_cs_name("id"), "id");
}

#[test]
fn native_methods_class() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("internal static class NativeMethods"),
        "missing NativeMethods: {cs}"
    );
    assert!(
        cs.contains("weaveffi_free_string"),
        "missing free_string P/Invoke: {cs}"
    );
    assert!(
        cs.contains("weaveffi_free_bytes"),
        "missing free_bytes P/Invoke: {cs}"
    );
}

#[test]
fn pinvoke_has_error_param() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("ref WeaveFFIError err"),
        "missing error param in P/Invoke: {cs}"
    );
}

#[test]
fn header_has_using_statements() {
    let api = make_api(vec![]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(cs.contains("using System;"), "missing System: {cs}");
    assert!(
        cs.contains("using System.Runtime.InteropServices;"),
        "missing InteropServices: {cs}"
    );
    assert!(
        cs.contains("using System.Collections.Generic;"),
        "missing Collections.Generic: {cs}"
    );
}

#[test]
fn optional_types() {
    assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::Bool))),
        "bool?"
    );
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        "string?"
    );
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::Enum("Foo".into())))),
        "Foo?"
    );
    assert_eq!(
        cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("Bar".into())))),
        "Bar?"
    );
}

#[test]
fn struct_return_decodes_value_buffer() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "get_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::Handle,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static Contact GetContact(ulong id)"),
        "missing method sig: {cs}"
    );
    // A record return arrives as a producer-owned value buffer: the
    // wrapper copies it, frees it, then decodes the copy.
    assert!(cs.contains("out var outLen"), "missing outLen slot: {cs}");
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "missing buffer release: {cs}"
    );
    assert!(
        cs.contains("var value = Contact.ReadFrom(valueReader);")
            && cs.contains("valueReader.ExpectEnd();")
            && cs.contains("return value;"),
        "missing buffer decode: {cs}"
    );
}

#[test]
fn list_return_type() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "get_ids".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static int[] GetIds()"),
        "missing list return method: {cs}"
    );
    assert!(cs.contains("out var outLen"), "missing outLen: {cs}");
    // The list crosses as one value buffer, not parallel arrays: the
    // wrapper copies it, frees it, then decodes count-prefixed elements.
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "missing value-buffer release: {cs}"
    );
    assert!(
        cs.contains("var valueCount = valueReader.ReadLen();")
            && cs.contains("var value = new int[valueCount];")
            && cs.contains("var valueItem = valueReader.ReadI32();"),
        "missing list decode loop: {cs}"
    );
}

#[test]
fn map_return_type() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "get_scores".into(),
            params: vec![],
            returns: Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static Dictionary<int, double> GetScores()"),
        "missing map return: {cs}"
    );
    // Parallel key/value buffers are gone: the map crosses as one value
    // buffer decoded as count-prefixed alternating pairs.
    assert!(
        !cs.contains("out var outKeys") && !cs.contains("out var outValues"),
        "parallel buffers must be gone: {cs}"
    );
    assert!(
        cs.contains("var value = new Dictionary<int, double>(valueCount);")
            && cs.contains("var valueKey = valueReader.ReadI32();")
            && cs.contains("var valueVal = valueReader.ReadF64();")
            && cs.contains("value[valueKey] = valueVal;"),
        "missing map decode loop: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "missing value-buffer release: {cs}"
    );
}

#[test]
fn struct_optional_string_field() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "email".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public string? Email { get; }"),
        "missing optional string property: {cs}"
    );
    // The optional encodes as a flag byte plus the value when present,
    // and decodes back into a nullable local.
    assert!(
        cs.contains("if (Email != null)")
            && cs.contains("writer.WriteOptionFlag(true);")
            && cs.contains("writer.WriteString(Email!);")
            && cs.contains("writer.WriteOptionFlag(false);"),
        "missing optional encode: {cs}"
    );
    assert!(
        cs.contains("string? fEmail = null;")
            && cs.contains("if (reader.ReadOptionFlag())")
            && cs.contains("var fEmailValue = reader.ReadString();"),
        "missing optional decode: {cs}"
    );
}

#[test]
fn optional_string_param_marshalling() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "create".into(),
            params: vec![
                Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::Handle),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Plain strings still cross as C strings.
    assert!(
        cs.contains("StringToCoTaskMemUTF8(name)"),
        "missing name marshal: {cs}"
    );
    assert!(
        cs.contains("FreeCoTaskMem(namePtr)"),
        "missing name cleanup: {cs}"
    );
    // The optional string is buffered: flag byte plus value, pinned and
    // passed as (ptr, len), then unpinned.
    assert!(
        cs.contains("var emailWriter = new WeaveFFIBufferWriter();")
            && cs.contains("if (email != null)")
            && cs.contains("emailWriter.WriteOptionFlag(true);")
            && cs.contains("emailWriter.WriteString(email!);"),
        "missing optional buffer encode: {cs}"
    );
    assert!(
        cs.contains("emailPin.AddrOfPinnedObject(), (UIntPtr)emailBuf.Length"),
        "missing (ptr, len) call args: {cs}"
    );
    assert!(cs.contains("emailPin.Free();"), "missing unpin: {cs}");
}

#[test]
fn comprehensive_contacts_api() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
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
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: None,
                },
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "contact_type".into(),
                    ty: TypeRef::Enum("ContactType".into()),
                    doc: None,
                },
            ],
        }],
        functions: vec![
            Function {
                name: "create_contact".into(),
                params: vec![
                    Param {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "count_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
        ],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_contacts_v2");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator
        .generate(
            &api,
            out_dir,
            &DotnetConfig {
                strip_module_prefix: true,
                ..DotnetConfig::default()
            },
        )
        .unwrap();

    let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

    assert!(cs.contains("public enum ContactType"));
    assert!(cs.contains("Personal = 0"));
    assert!(cs.contains("Work = 1"));

    // The record is a plain value class with typed properties and the
    // value-buffer pack/unpack pair; no handle or C symbols remain.
    assert!(cs.contains("public sealed class Contact"));
    assert!(cs.contains(
        "public Contact(long id, string firstName, string? email, ContactType contactType)"
    ));
    assert!(cs.contains("public long Id { get; }"));
    assert!(cs.contains("public string FirstName { get; }"));
    assert!(cs.contains("public string? Email { get; }"));
    assert!(cs.contains("public ContactType ContactType { get; }"));
    assert!(cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)"));
    assert!(cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"));
    assert!(!cs.contains("weaveffi_contacts_Contact_"));

    assert!(cs.contains("weaveffi_contacts_create_contact("));
    assert!(cs.contains("weaveffi_contacts_get_contact("));
    assert!(cs.contains("weaveffi_contacts_count_contacts("));

    assert!(cs.contains("public static class Contacts"));
    assert!(cs.contains("public static ulong CreateContact("));
    assert!(cs.contains("public static Contact GetContact("));
    assert!(cs.contains("public static int CountContacts("));

    assert!(cs.contains("internal static class NativeMethods"));
    assert!(cs.contains("CallingConvention.Cdecl"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_dotnet_basic() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_basic");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator
        .generate(
            &api,
            out_dir,
            &DotnetConfig {
                strip_module_prefix: true,
                ..DotnetConfig::default()
            },
        )
        .unwrap();
    let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

    assert!(
        cs.contains("EntryPoint = \"weaveffi_math_add\""),
        "missing P/Invoke EntryPoint: {cs}"
    );
    assert!(
        cs.contains(
            "internal static extern int weaveffi_math_add(int a, int b, ref WeaveFFIError err)"
        ),
        "missing P/Invoke declaration: {cs}"
    );
    assert!(
        cs.contains("public static int Add(int a, int b)"),
        "missing wrapper method signature: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_math_add(a, b, ref err)"),
        "missing P/Invoke call in wrapper: {cs}"
    );
    assert!(
        cs.contains("WeaveFFIError.Check(err)"),
        "missing error check in wrapper: {cs}"
    );
    assert!(
        cs.contains("return result;"),
        "missing return statement: {cs}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_dotnet_with_structs() {
    let api = make_api(vec![Module {
        name: "crm".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Person".into(),
            doc: Some("A person record".into()),
            fields: vec![
                StructField {
                    name: "full_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
                StructField {
                    name: "score".into(),
                    ty: TypeRef::F64,
                    doc: None,
                },
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );

    assert!(
        cs.contains("public sealed class Person"),
        "missing sealed value class: {cs}"
    );
    assert!(
        cs.contains("<summary>A person record</summary>"),
        "missing doc summary: {cs}"
    );
    assert!(
        cs.contains("public Person(string fullName, int age, double score, bool active)"),
        "missing positional constructor: {cs}"
    );

    assert!(
        cs.contains("public string FullName { get; }"),
        "missing FullName property: {cs}"
    );
    assert!(
        cs.contains("public int Age { get; }"),
        "missing Age property: {cs}"
    );
    assert!(
        cs.contains("public double Score { get; }"),
        "missing Score property: {cs}"
    );
    assert!(
        cs.contains("public bool Active { get; }"),
        "missing Active property: {cs}"
    );

    // The pack/unpack pair covers every field in declaration order.
    assert!(
        cs.contains("writer.WriteString(FullName);")
            && cs.contains("writer.WriteI32(Age);")
            && cs.contains("writer.WriteF64(Score);")
            && cs.contains("writer.WriteBool(Active);"),
        "missing field encodings: {cs}"
    );
    assert!(
        cs.contains("var fFullName = reader.ReadString();")
            && cs.contains("var fAge = reader.ReadI32();")
            && cs.contains("var fScore = reader.ReadF64();")
            && cs.contains("var fActive = reader.ReadBool();")
            && cs.contains("return new Person(fFullName, fAge, fScore, fActive);"),
        "missing field decodings: {cs}"
    );

    // No native lifecycle remains for records.
    assert!(
        !cs.contains("weaveffi_crm_Person_") && !cs.contains("~Person()"),
        "record C symbols must be gone: {cs}"
    );
}

#[test]
fn generate_dotnet_with_enums() {
    let api = make_api(vec![Module {
        name: "status".into(),
        functions: vec![Function {
            name: "get_status".into(),
            params: vec![],
            returns: Some(TypeRef::Enum("Priority".into())),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Priority".into(),
            doc: Some("Task priority levels".into()),
            variants: vec![
                EnumVariant {
                    name: "Low".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Medium".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "High".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Critical".into(),
                    value: 3,
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

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );

    assert!(
        cs.contains("<summary>Task priority levels</summary>"),
        "missing enum doc: {cs}"
    );
    assert!(
        cs.contains("public enum Priority"),
        "missing enum declaration: {cs}"
    );
    assert!(cs.contains("Low = 0,"), "missing Low variant: {cs}");
    assert!(cs.contains("Medium = 1,"), "missing Medium variant: {cs}");
    assert!(cs.contains("High = 2,"), "missing High variant: {cs}");
    assert!(
        cs.contains("Critical = 3,"),
        "missing Critical variant: {cs}"
    );

    assert!(
        cs.contains("(Priority)result"),
        "missing enum return cast: {cs}"
    );
    assert!(
        cs.contains("public static Priority GetStatus()"),
        "missing wrapper returning enum: {cs}"
    );
}

#[test]
fn generate_dotnet_with_optionals() {
    let api = make_api(vec![Module {
        name: "config".into(),
        functions: vec![Function {
            name: "update".into(),
            params: vec![
                Param {
                    name: "label".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "count".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::I64))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Settings".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "nickname".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "max_retries".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    doc: None,
                },
                StructField {
                    name: "threshold".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::F64)),
                    doc: None,
                },
                StructField {
                    name: "enabled".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Bool)),
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );

    assert!(
        cs.contains("public static long? Update(string? label, int? count)"),
        "missing Nullable wrapper sig: {cs}"
    );

    // Optional parameters are buffered: a flag byte plus the value when
    // present, pinned and passed as (ptr, len).
    assert!(
        cs.contains("var labelWriter = new WeaveFFIBufferWriter();")
            && cs.contains("labelWriter.WriteString(label!);"),
        "missing optional string param encode: {cs}"
    );
    assert!(
        cs.contains("var countWriter = new WeaveFFIBufferWriter();")
            && cs.contains("countWriter.WriteI32(count.Value);"),
        "missing optional int param encode: {cs}"
    );

    // The optional return decodes from the freed-after-copy value buffer.
    assert!(
        cs.contains("long? value = null;")
            && cs.contains("if (valueReader.ReadOptionFlag())")
            && cs.contains("var valueValue = valueReader.ReadI64();"),
        "missing optional return decode: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "missing return buffer release: {cs}"
    );

    // Optional record fields become nullable properties with flag-byte
    // encodings; no boxed-scalar pointers remain.
    assert!(
        cs.contains("public string? Nickname { get; }"),
        "missing optional string property: {cs}"
    );
    assert!(
        cs.contains("public int? MaxRetries { get; }"),
        "missing optional int property: {cs}"
    );
    assert!(
        cs.contains("public double? Threshold { get; }"),
        "missing optional f64 property: {cs}"
    );
    assert!(
        cs.contains("public bool? Enabled { get; }"),
        "missing optional bool property: {cs}"
    );
    assert!(
        cs.contains("writer.WriteF64(Threshold.Value);")
            && cs.contains("writer.WriteBool(Enabled.Value);"),
        "missing optional field encodings: {cs}"
    );
    assert!(
        cs.contains("bool? fEnabled = null;") && cs.contains("double? fThreshold = null;"),
        "missing optional field decodings: {cs}"
    );
    assert!(
        !cs.contains("Marshal.ReadByte(ptr)"),
        "boxed-scalar pointers must be gone: {cs}"
    );
}

#[test]
fn generate_dotnet_with_lists() {
    let api = make_api(vec![Module {
        name: "data".into(),
        functions: vec![
            Function {
                name: "get_ids".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_values".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::F64))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_timestamps".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I64))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![StructDef {
            name: "Record".into(),
            doc: None,
            fields: vec![StructField {
                name: "tags".into(),
                ty: TypeRef::List(Box::new(TypeRef::I32)),
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );

    assert!(
        cs.contains("public static int[] GetIds()"),
        "missing int[] return: {cs}"
    );
    assert!(
        cs.contains("public static double[] GetValues()"),
        "missing double[] return: {cs}"
    );
    assert!(
        cs.contains("public static long[] GetTimestamps()"),
        "missing long[] return: {cs}"
    );
    assert!(
        cs.contains("out var outLen"),
        "missing outLen parameter: {cs}"
    );
    // Each list decodes from its own value buffer: count prefix, typed
    // elements, then the producer buffer is released.
    assert!(
        cs.contains("var value = new int[valueCount];")
            && cs.contains("var value = new double[valueCount];")
            && cs.contains("var value = new long[valueCount];"),
        "missing typed element arrays: {cs}"
    );
    assert!(
        cs.contains("var valueItem = valueReader.ReadI32();")
            && cs.contains("var valueItem = valueReader.ReadF64();")
            && cs.contains("var valueItem = valueReader.ReadI64();"),
        "missing element decodes: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "missing value-buffer release: {cs}"
    );

    // A list-typed record field is a plain typed property with a
    // count-prefixed encoding.
    assert!(
        cs.contains("public int[] Tags { get; }"),
        "missing list property: {cs}"
    );
    assert!(
        cs.contains("writer.WriteLen(Tags.Length);"),
        "missing list field encode: {cs}"
    );
}

#[test]
fn generate_dotnet_full_contacts() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        enums: vec![EnumDef {
            name: "ContactType".into(),
            doc: Some("Type of contact".into()),
            variants: vec![
                EnumVariant {
                    name: "Personal".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Business".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Government".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: Some("A contact entry".into()),
            fields: vec![
                StructField {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    doc: None,
                },
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "last_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                },
                StructField {
                    name: "contact_type".into(),
                    ty: TypeRef::Enum("ContactType".into()),
                    doc: None,
                },
                StructField {
                    name: "scores".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    doc: None,
                },
            ],
        }],
        functions: vec![
            Function {
                name: "create_contact".into(),
                params: vec![
                    Param {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "list_contacts".into(),
                params: vec![Param {
                    name: "contact_type".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Enum("ContactType".into()))),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "delete_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "count_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
        ],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_full_contacts");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator
        .generate(
            &api,
            out_dir,
            &DotnetConfig {
                strip_module_prefix: true,
                ..DotnetConfig::default()
            },
        )
        .unwrap();
    let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

    // Enum
    assert!(cs.contains("public enum ContactType"), "missing enum: {cs}");
    assert!(cs.contains("Personal = 0,"), "missing Personal: {cs}");
    assert!(cs.contains("Business = 1,"), "missing Business: {cs}");
    assert!(cs.contains("Government = 2,"), "missing Government: {cs}");
    assert!(
        cs.contains("<summary>Type of contact</summary>"),
        "missing enum doc: {cs}"
    );

    // Struct as a plain value class
    assert!(
        cs.contains("public sealed class Contact"),
        "missing sealed value class: {cs}"
    );
    assert!(
        cs.contains("<summary>A contact entry</summary>"),
        "missing struct doc: {cs}"
    );
    assert!(
        !cs.contains("~Contact()") && !cs.contains("weaveffi_contacts_Contact_"),
        "record lifecycle symbols must be gone: {cs}"
    );

    // Typed properties
    assert!(
        cs.contains("public ulong Id { get; }"),
        "missing Id property: {cs}"
    );
    assert!(
        cs.contains("public string FirstName { get; }"),
        "missing FirstName: {cs}"
    );
    assert!(
        cs.contains("public string LastName { get; }"),
        "missing LastName: {cs}"
    );
    assert!(
        cs.contains("public string? Email { get; }"),
        "missing optional Email: {cs}"
    );
    assert!(cs.contains("public int Age { get; }"), "missing Age: {cs}");
    assert!(
        cs.contains("public bool Active { get; }"),
        "missing Active: {cs}"
    );
    assert!(
        cs.contains("public ContactType ContactType { get; }"),
        "missing ContactType property: {cs}"
    );
    assert!(
        cs.contains("public int[] Scores { get; }"),
        "missing Scores list property: {cs}"
    );

    // Pack/unpack pair
    assert!(
        cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
            && cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"),
        "missing pack/unpack pair: {cs}"
    );

    // P/Invoke declarations
    assert!(
        cs.contains("weaveffi_contacts_create_contact("),
        "missing create_contact P/Invoke: {cs}"
    );
    assert!(
        cs.contains("weaveffi_contacts_get_contact("),
        "missing get_contact P/Invoke: {cs}"
    );
    assert!(
        cs.contains("weaveffi_contacts_list_contacts("),
        "missing list_contacts P/Invoke: {cs}"
    );
    assert!(
        cs.contains("weaveffi_contacts_delete_contact("),
        "missing delete_contact P/Invoke: {cs}"
    );
    assert!(
        cs.contains("weaveffi_contacts_count_contacts("),
        "missing count_contacts P/Invoke: {cs}"
    );

    // Wrapper class
    assert!(
        cs.contains("public static class Contacts"),
        "missing Contacts wrapper class: {cs}"
    );
    assert!(
        cs.contains("public static ulong CreateContact("),
        "missing CreateContact wrapper: {cs}"
    );
    assert!(
        cs.contains("public static Contact GetContact(ulong id)"),
        "missing GetContact wrapper: {cs}"
    );
    assert!(
        cs.contains("public static Contact[] ListContacts("),
        "missing ListContacts wrapper: {cs}"
    );
    assert!(
        cs.contains("public static void DeleteContact(ulong id)"),
        "missing DeleteContact wrapper: {cs}"
    );
    assert!(
        cs.contains("public static int CountContacts()"),
        "missing CountContacts wrapper: {cs}"
    );

    // Supporting output files
    assert!(
        tmp.join("dotnet/WeaveFFI.csproj").exists(),
        ".csproj must exist"
    );
    assert!(
        tmp.join("dotnet/WeaveFFI.nuspec").exists(),
        ".nuspec must exist"
    );
    assert!(tmp.join("dotnet/README.md").exists(), "README must exist");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dotnet_has_memory_helpers() {
    let api = make_api(vec![simple_module(vec![])]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("internal static class WeaveFFIHelpers"),
        "missing WeaveFFIHelpers class: {cs}"
    );
    assert!(
        cs.contains("internal static IntPtr StringToPtr(string? s)"),
        "missing StringToPtr: {cs}"
    );
    assert!(
        cs.contains("internal static string? PtrToString(IntPtr ptr)"),
        "missing PtrToString: {cs}"
    );
    assert!(
        cs.contains("internal static void FreePtr(IntPtr ptr)"),
        "missing FreePtr: {cs}"
    );
    assert!(
        cs.contains("Marshal.StringToCoTaskMemUTF8(s)"),
        "missing StringToCoTaskMemUTF8 in helper: {cs}"
    );
    assert!(
        cs.contains("Marshal.FreeCoTaskMem(ptr)"),
        "missing FreeCoTaskMem in helper: {cs}"
    );
}

#[test]
fn dotnet_custom_namespace() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let config = DotnetConfig {
        namespace: Some("MyCompany.Bindings".into()),
        ..DotnetConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_custom_ns");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator.generate(&api, out_dir, &config).unwrap();

    let cs_path = tmp.join("dotnet/MyCompany.Bindings.cs");
    assert!(
        cs_path.exists(),
        ".cs file should use custom namespace name"
    );
    let cs = std::fs::read_to_string(&cs_path).unwrap();
    assert!(
        cs.contains("namespace MyCompany.Bindings"),
        "namespace should use custom name: {cs}"
    );

    let csproj_path = tmp.join("dotnet/MyCompany.Bindings.csproj");
    assert!(csproj_path.exists(), ".csproj should use custom namespace");
    let csproj = std::fs::read_to_string(&csproj_path).unwrap();
    assert!(
        csproj.contains("<PackageId>MyCompany.Bindings</PackageId>"),
        "PackageId should use custom namespace: {csproj}"
    );

    let nuspec_path = tmp.join("dotnet/MyCompany.Bindings.nuspec");
    assert!(nuspec_path.exists(), ".nuspec should use custom namespace");
    let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
    assert!(
        nuspec.contains("<id>MyCompany.Bindings</id>"),
        "nuspec id should use custom namespace: {nuspec}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn dotnet_strip_module_prefix() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "create_contact".into(),
            params: vec![Param {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);

    // Stripping is the default: the per-module static class already
    // namespaces the method.
    let config = DotnetConfig::default();

    let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    DotnetGenerator.generate(&api, out_dir, &config).unwrap();

    let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
    assert!(
        cs.contains("CreateContact("),
        "stripped name should be CreateContact: {cs}"
    );
    assert!(
        !cs.contains("ContactsCreateContact("),
        "should not contain module-prefixed name: {cs}"
    );
    assert!(
        cs.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {cs}"
    );

    let no_strip = DotnetConfig {
        strip_module_prefix: false,
        ..DotnetConfig::default()
    };
    let tmp2 = std::env::temp_dir().join("weaveffi_test_dotnet_no_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp2);
    std::fs::create_dir_all(&tmp2).unwrap();
    let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

    DotnetGenerator.generate(&api, out_dir2, &no_strip).unwrap();

    let cs2 = std::fs::read_to_string(tmp2.join("dotnet/WeaveFFI.cs")).unwrap();
    assert!(
        cs2.contains("ContactsCreateContact("),
        "strip_module_prefix: false should restore module-prefixed names: {cs2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&tmp2);
}

#[test]
fn dotnet_deeply_nested_optional() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "data".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                    Box::new(TypeRef::Record("Contact".into())),
                ))))),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("Contact?[]?"),
        "should contain deeply nested optional type: {cs}"
    );
}

#[test]
fn dotnet_map_of_lists() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "scores".into(),
                ty: TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                ),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("Dictionary<string, int[]>"),
        "should contain map of lists type: {cs}"
    );
}

#[test]
fn dotnet_enum_keyed_map() {
    let api = make_api(vec![Module {
        name: "edge".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "contacts".into(),
                ty: TypeRef::Map(
                    Box::new(TypeRef::Enum("Color".into())),
                    Box::new(TypeRef::Record("Contact".into())),
                ),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
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
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("Dictionary<Color, Contact>"),
        "should contain enum-keyed map type: {cs}"
    );
}

#[test]
fn dotnet_typed_handle_type() {
    let api = ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules: vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }],
        generators: None,
        package: None,
    });
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // A typed handle renders as a dedicated readonly wrapper struct, not
    // a bare ulong and not the record class.
    assert!(
        cs.contains("ContactHandle contact"),
        "TypedHandle should use the wrapper struct: {cs}"
    );
    assert!(
        cs.contains("public readonly struct ContactHandle"),
        "missing handle wrapper struct: {cs}"
    );
    assert!(
        cs.contains("contact.Raw"),
        "wrapper should pass the raw pointer: {cs}"
    );
}

#[test]
fn dotnet_no_double_free_on_error() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("StringToCoTaskMemUTF8"),
        "string param should be marshalled to unmanaged memory: {cs}"
    );
    assert!(
        cs.contains("finally") && cs.contains("FreeCoTaskMem"),
        "marshalled string should be freed in finally (no double-free of managed string): {cs}"
    );
    let find = cs.find("FindContact").expect("FindContact wrapper");
    let slice = &cs[find..];
    let check_rel = slice
        .find("WeaveFFIError.Check(err)")
        .expect("WeaveFFIError.Check in FindContact");
    let free_rel = slice
        .find("NativeMethods.weaveffi_free_bytes(result, outLen);")
        .expect("value-buffer release in FindContact");
    let decode_rel = slice
        .find("Contact.ReadFrom(valueReader)")
        .expect("Contact.ReadFrom in FindContact");
    assert!(
        check_rel < free_rel && free_rel < decode_rel,
        "error must be checked, the buffer freed once, then decoded: {cs}"
    );
    // The record is a value type: nothing to dispose, so no double-free
    // hazard on the return path.
    assert!(
        !cs.contains("Contact : IDisposable") && !cs.contains("~Contact()"),
        "record must not be disposable: {cs}"
    );
}

#[test]
fn dotnet_null_check_on_optional_return() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // The optional record decodes from the value buffer: a flag byte
    // gates the nested record read, and absent means null.
    assert!(
        cs.contains("public static Contact? FindContact(int id)"),
        "missing nullable wrapper sig: {cs}"
    );
    assert!(
        cs.contains("Contact? value = null;")
            && cs.contains("if (valueReader.ReadOptionFlag())")
            && cs.contains("var valueValue = Contact.ReadFrom(valueReader);"),
        "optional record return should decode via flag byte: {cs}"
    );
}

#[test]
fn dotnet_async_returns_task() {
    let api = make_api(vec![Module {
        name: "tasks".into(),
        functions: vec![Function {
            name: "run".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("async Task<"),
        "missing async Task< in signature: {cs}"
    );
}

#[test]
fn dotnet_async_uses_tcs() {
    let api = make_api(vec![Module {
        name: "tasks".into(),
        functions: vec![Function {
            name: "run".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("TaskCompletionSource"),
        "missing TaskCompletionSource: {cs}"
    );
}

/// `GCHandle.Alloc(callback, GCHandleType.Normal)` (the .NET equivalent
/// of pinning the delegate so the GC won't reclaim it while the C side
/// owns a function pointer to it) must be balanced by exactly one
/// `GCHandle.FromIntPtr(context).Free()` in the C callback after the
/// `TaskCompletionSource` is resolved.
#[test]
fn dotnet_async_pins_callback_for_lifetime() {
    let api = make_api(vec![Module {
        name: "tasks".into(),
        functions: vec![Function {
            name: "run".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("GCHandle.Alloc(callback, GCHandleType.Normal)"),
        "missing GCHandle.Alloc(..., Normal): {cs}"
    );
    assert!(
        cs.contains("GCHandle.ToIntPtr(gcHandle)"),
        "GCHandle must be passed as the C context: {cs}"
    );
    assert!(
        cs.contains("GCHandle.FromIntPtr(context).Free()"),
        "missing GCHandle.Free in callback: {cs}"
    );
}

/// A module with one async function per given return type, named `run0`,
/// `run1`, ... in order, plus a `Contact` record for object results.
fn async_api(returns: Vec<Option<TypeRef>>) -> ResolvedApi {
    let functions = returns
        .into_iter()
        .enumerate()
        .map(|(i, ret)| Function {
            name: format!("run{i}"),
            params: vec![],
            returns: ret,
            doc: None,
            r#async: true,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        })
        .collect();
    make_api(vec![Module {
        name: "tasks".into(),
        functions,
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::Handle,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }])
}

/// Async result buffers are owned by the consumer (`AsyncProtocol`
/// clause 2): strings and bytes are deep-copied inside the callback and
/// then released through the runtime free symbols.
#[test]
fn dotnet_async_owned_results_copied_then_freed() {
    let cs = render_csharp(
        &async_api(vec![
            Some(TypeRef::StringUtf8),
            Some(TypeRef::Bytes),
            Some(TypeRef::Optional(Box::new(TypeRef::I64))),
        ]),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // String result: copied, then freed.
    assert!(
        cs.contains("var str = Marshal.PtrToStringUTF8(result) ?? \"\";")
            && cs.contains("tcs.SetResult(str);"),
        "async string result must copy: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_string(result);"),
        "async string result is owned and must be freed after copying: {cs}"
    );
    // Bytes result: copied via the (result, resultLen) pair, then freed.
    assert!(
        cs.contains("Marshal.Copy(result, arr, 0, (int)resultLen);"),
        "async bytes result must copy: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, resultLen);"),
        "async bytes result is owned and must be freed after copying: {cs}"
    );
    // Buffered optional result: the owned buffer is copied, freed, and
    // decoded inside the callback.
    assert!(
        cs.contains("Marshal.Copy(result, resultBuf, 0, (int)resultLen);")
            && cs.contains("long? value = null;")
            && cs.contains("var valueValue = valueReader.ReadI64();")
            && cs.contains("tcs.SetResult(value);"),
        "async optional result must decode the owned buffer: {cs}"
    );
}

/// Record, list, and map async results all arrive as one owned
/// `(result, resultLen)` value buffer: the callback copies, frees, and
/// decodes it before completing the task.
#[test]
fn dotnet_async_buffered_results_decoded() {
    let cs = render_csharp(
        &async_api(vec![
            Some(TypeRef::Record("Contact".into())),
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        ]),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Every buffered result arrives through the (result, resultLen) pair.
    assert!(
        cs.contains("IntPtr result, UIntPtr resultLen"),
        "async buffered delegate must carry the length slot: {cs}"
    );
    // Record result decoded from the local copy.
    assert!(
        cs.contains("var value = Contact.ReadFrom(valueReader);")
            && cs.contains("tcs.SetResult(value);"),
        "async record result must decode: {cs}"
    );
    // List elements decode in place: strings copy, records recurse.
    assert!(
        cs.contains("var valueItem = valueReader.ReadString();"),
        "async string list elements must decode: {cs}"
    );
    assert!(
        cs.contains("var valueItem = Contact.ReadFrom(valueReader);"),
        "async record list elements must decode: {cs}"
    );
    // Map results decode from the same single buffer as alternating
    // key/value pairs; no parallel buffers remain.
    assert!(
        cs.contains("var value = new Dictionary<string, int>(valueCount);")
            && cs.contains("var valueKey = valueReader.ReadString();")
            && cs.contains("var valueVal = valueReader.ReadI32();"),
        "async map result must decode pairs: {cs}"
    );
    assert!(
        !cs.contains("resultKeys") && !cs.contains("resultValues"),
        "parallel map buffers must be gone: {cs}"
    );
    // Every owned value buffer is released after the copy.
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, resultLen);"),
        "async result buffers are owned and must be freed after copying: {cs}"
    );
}

/// The iterator contract (`IteratorProtocol`): the sequence streams
/// through a single `yield return` enumerator (one C `next` per
/// `MoveNext`), frees each string element after conversion, destroys the
/// native iterator exactly once from the compiler-generated `finally`,
/// and refuses a second enumeration instead of double-destroying.
#[test]
fn iterator_streams_lazily_and_destroys_once() {
    let cs = render_csharp(
        &kv_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // The single-use wrapper class and the wrapped return.
    assert!(
        cs.contains("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>"),
        "once-enumerable class missing: {cs}"
    );
    assert!(
        cs.contains("return new WeaveFFIOnceEnumerable<string>(EnumerateListKeys(iter));"),
        "iterator wrapper must return the once-enumerable: {cs}"
    );
    assert!(
        cs.contains("this sequence can be enumerated only once"),
        "second enumeration must throw: {cs}"
    );
    // One C next call per MoveNext, inside a lazy yield-return method.
    assert_eq!(
        cs.matches("weaveffi_kv_Store_ListKeysIterator_next(iter, out var out_item, ref iterErr)")
            .count(),
        1,
        "exactly one next call site expected: {cs}"
    );
    assert!(
        cs.contains("yield return item;"),
        "enumerator must stream lazily: {cs}"
    );
    // Each yielded string is freed after conversion (ElemFree::String).
    assert!(
        cs.contains("NativeMethods.weaveffi_free_string(out_item);"),
        "string elements must be freed: {cs}"
    );
    // Destroy exactly once, from the enumerator's finally (which C#'s
    // foreach reaches through Dispose() on early abandonment too).
    assert_eq!(
        cs.matches("NativeMethods.weaveffi_kv_Store_ListKeysIterator_destroy(iter);")
            .count(),
        1,
        "exactly one destroy call site expected: {cs}"
    );
    assert!(cs.contains("finally"), "destroy must run in finally: {cs}");
}

/// A list-of-strings return arrives as one value buffer: the strings
/// decode from the copy and the producer buffer is released exactly once
/// with `weaveffi_free_bytes`; no per-element frees remain.
#[test]
fn string_list_return_decodes_and_frees_buffer_once() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "names".into(),
        params: vec![],
        returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("var value = new string[valueCount];")
            && cs.contains("var valueItem = valueReader.ReadString();"),
        "string elements must decode from the buffer: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
        "value buffer must be released: {cs}"
    );
    let names = cs.find("static string[] Names()").expect("Names wrapper");
    assert!(
        !cs[names..].contains("weaveffi_free_string("),
        "no per-element frees may remain in the wrapper: {cs}"
    );
}

#[test]
fn dotnet_nested_module_output() {
    let api = make_api(vec![Module {
        name: "parent".to_string(),
        functions: vec![Function {
            name: "outer_fn".to_string(),
            params: vec![],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![Module {
            name: "child".to_string(),
            functions: vec![Function {
                name: "inner_fn".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static class Parent"),
        "top-level wrapper class missing: {cs}"
    );
    assert!(
        cs.contains("public static class ParentChild"),
        "submodule wrapper class must be flattened to its full path: {cs}"
    );
    assert!(
        cs.contains("weaveffi_parent_outer_fn"),
        "parent P/Invoke missing: {cs}"
    );
    assert!(
        cs.contains("weaveffi_parent_child_inner_fn"),
        "nested child P/Invoke missing: {cs}"
    );
}

#[test]
fn deprecated_function_generates_annotation() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add_old".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: Some("Use AddV2 instead".into()),
        since: Some("0.1.0".into()),
    }])]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("[Obsolete(\"Use AddV2 instead\")]"),
        "missing Obsolete attribute: {cs}"
    );
}

fn doc_api() -> ResolvedApi {
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
    }])
}

#[test]
fn dotnet_emits_doc_on_function() {
    let cs = render_csharp(
        &doc_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("/// <summary>Performs a thing.</summary>"),
        "{cs}"
    );
}

#[test]
fn dotnet_emits_doc_on_struct() {
    let cs = render_csharp(
        &doc_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("/// <summary>An item we track.</summary>"),
        "{cs}"
    );
}

#[test]
fn dotnet_emits_doc_on_enum_variant() {
    let cs = render_csharp(
        &doc_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(cs.contains("/// <summary>Kind of item.</summary>"), "{cs}");
    assert!(cs.contains("/// <summary>A small one</summary>"), "{cs}");
}

#[test]
fn dotnet_emits_doc_on_field() {
    let cs = render_csharp(
        &doc_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(cs.contains("/// <summary>Stable id</summary>"), "{cs}");
}

#[test]
fn dotnet_emits_doc_on_param() {
    let cs = render_csharp(
        &doc_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("/// <param name=\"x\">the input value</param>"),
        "{cs}"
    );
}

#[test]
fn dotnet_custom_prefix_threads_to_user_symbols() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "add".into(),
        params: vec![
            Param {
                name: "a".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
            Param {
                name: "b".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            },
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }])]);

    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "myffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );

    // User symbols pick up the configured ABI prefix...
    assert!(
        cs.contains("myffi_math_add"),
        "user symbol must honor the custom prefix: {cs}"
    );
    assert!(
        !cs.contains("weaveffi_math_add"),
        "user symbol must not retain the default prefix: {cs}"
    );
    // ...while runtime ABI helpers stay literally `weaveffi_*`.
    assert!(
        cs.contains("weaveffi_free_string"),
        "runtime ABI helper must stay literal: {cs}"
    );
}

fn shapes_api() -> ResolvedApi {
    let shape = EnumDef {
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
                    doc: None,
                }],
            },
            EnumVariant {
                name: "Rectangle".into(),
                value: 2,
                doc: None,
                fields: vec![
                    StructField {
                        name: "width".into(),
                        ty: TypeRef::F32,
                        doc: None,
                    },
                    StructField {
                        name: "height".into(),
                        ty: TypeRef::F32,
                        doc: None,
                    },
                ],
            },
            EnumVariant {
                name: "Labeled".into(),
                value: 3,
                doc: None,
                fields: vec![
                    StructField {
                        name: "label".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "count".into(),
                        ty: TypeRef::U8,
                        doc: None,
                    },
                ],
            },
        ],
    };
    let channel = EnumDef {
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
    };
    make_api(vec![Module {
        name: "shapes".into(),
        functions: vec![
            Function {
                name: "describe".into(),
                params: vec![Param {
                    name: "shape".into(),
                    ty: TypeRef::RichEnum("Shape".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "scale".into(),
                params: vec![
                    Param {
                        name: "shape".into(),
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "factor".into(),
                        ty: TypeRef::F64,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::RichEnum("Shape".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "sum_bytes".into(),
                params: vec![Param {
                    name: "values".into(),
                    ty: TypeRef::List(Box::new(TypeRef::U8)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::U64),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![shape, channel],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }])
}

#[test]
fn rich_enum_generates_sum_type() {
    let cs = render_csharp(
        &shapes_api(),
        "Shapes",
        false,
        "weaveffi",
        "shapes.yml",
        "Shapes.cs",
    );

    // Rich enum becomes an abstract sum type, not a C# enum and not a
    // disposable handle wrapper.
    assert!(
        cs.contains("public abstract class Shape"),
        "rich enum must be an abstract class: {cs}"
    );
    assert!(
        !cs.contains("public enum Shape"),
        "rich enum must not be a plain enum: {cs}"
    );
    assert!(
        !cs.contains("Shape : IDisposable"),
        "rich enum must not be disposable: {cs}"
    );
    // Plain enum is still a value enum.
    assert!(
        cs.contains("public enum Channel"),
        "plain enum must stay an enum: {cs}"
    );

    // One nested sealed class per variant, with typed properties and
    // positional constructors instead of factories and getters.
    assert!(
        cs.contains("public sealed class Empty : Shape"),
        "Empty variant class: {cs}"
    );
    assert!(
        cs.contains("public sealed class Circle : Shape")
            && cs.contains("public Circle(double radius)")
            && cs.contains("public double Radius { get; }"),
        "Circle variant class: {cs}"
    );
    assert!(
        cs.contains("public sealed class Rectangle : Shape")
            && cs.contains("public Rectangle(float width, float height)")
            && cs.contains("public float Width { get; }")
            && cs.contains("public float Height { get; }"),
        "Rectangle variant class: {cs}"
    );
    assert!(
        cs.contains("public sealed class Labeled : Shape")
            && cs.contains("public Labeled(string label, byte count)")
            && cs.contains("public string Label { get; }")
            && cs.contains("public byte Count { get; }"),
        "Labeled variant class: {cs}"
    );

    // The pack pair writes the i32 tag then the active variant's fields.
    assert!(
        cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
            && cs.contains("case Circle v:")
            && cs.contains("writer.WriteI32(1);")
            && cs.contains("writer.WriteF64(v.Radius);"),
        "tag-dispatched encode: {cs}"
    );
    assert!(
        cs.contains("internal static Shape ReadFrom(WeaveFFIBufferReader reader)")
            && cs.contains("var tag = reader.ReadI32();")
            && cs.contains("return new Empty();")
            && cs.contains("return new Labeled(fLabel, fCount);"),
        "tag-dispatched decode: {cs}"
    );

    // Rich enums declare no C symbols at all.
    assert!(
        !cs.contains("weaveffi_shapes_Shape_"),
        "rich enum C symbols must be gone: {cs}"
    );

    // Functions taking the enum pack it into a pinned value buffer and
    // pass (ptr, len); returns decode from the freed-after-copy buffer.
    assert!(
        cs.contains("public static string ShapesDescribe(Shape shape)")
            && cs.contains("shape.WriteTo(shapeWriter);")
            && cs.contains(
                "weaveffi_shapes_describe(shapePin.AddrOfPinnedObject(), (UIntPtr)shapeBuf.Length, ref err)"
            ),
        "describe via buffered param: {cs}"
    );
    assert!(
        cs.contains("public static Shape ShapesScale(Shape shape, double factor)")
            && cs.contains("var value = Shape.ReadFrom(valueReader);"),
        "scale via buffered return: {cs}"
    );
    // Numerics smoke: list<u8> in, u64 out (plain function path).
    assert!(
        cs.contains("public static ulong ShapesSumBytes(byte[] values)"),
        "sum_bytes wrapper: {cs}"
    );
}

/// A `kv` module exercising the 0.5.0 surface: a declared error domain, a
/// `Store` interface (real ctor, named factory, sync/iterator/async
/// methods, a static), and free functions with mixed `throws`.
fn kv_api() -> ResolvedApi {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};
    let f = |name: &str,
             params: Vec<Param>,
             returns: Option<TypeRef>,
             throws: bool,
             is_async: bool,
             cancellable: bool| Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws,
        r#async: is_async,
        cancellable,
        deprecated: None,
        since: None,
    };
    let p = |name: &str, ty: TypeRef| Param {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
    };
    make_api(vec![Module {
        name: "kv".into(),
        functions: vec![
            f(
                "lookup_store",
                vec![p("store", TypeRef::Interface("Store".into()))],
                Some(TypeRef::U64),
                true,
                false,
                false,
            ),
            f("ping", vec![], Some(TypeRef::Bool), false, false, false),
        ],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store.".into()),
            constructors: vec![
                f(
                    "new",
                    vec![p("path", TypeRef::StringUtf8)],
                    None,
                    true,
                    false,
                    false,
                ),
                f(
                    "open_readonly",
                    vec![p("path", TypeRef::StringUtf8)],
                    None,
                    false,
                    false,
                    false,
                ),
            ],
            methods: vec![
                f(
                    "get",
                    vec![p("store_key", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                    true,
                    false,
                    false,
                ),
                f("count", vec![], Some(TypeRef::U64), false, false, false),
                f(
                    "list_keys",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                    true,
                    false,
                    false,
                ),
                f("compact", vec![], None, true, true, true),
            ],
            statics: vec![f(
                "default_capacity",
                vec![],
                Some(TypeRef::U32),
                false,
                false,
                false,
            )],
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KEY_NOT_FOUND".into(),
                    code: 1001,
                    message: "Key not found".into(),
                    doc: None,
                    // Structured payload: the missing key and the attempt
                    // count, exercising the payload decode in FromCode.
                    fields: vec![
                        StructField {
                            name: "key".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "attempts".into(),
                            ty: TypeRef::I32,
                            doc: None,
                        },
                    ],
                },
                ErrorCode {
                    name: "IO_ERROR".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: Some("Underlying storage failed.".into()),
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }])
}

#[test]
fn typed_exception_rendering() {
    let cs = render_csharp(
        &kv_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // The domain exception extends the generic brand exception and drops
    // the doubled suffix (KvException, not KvErrorException).
    assert!(
        cs.contains("public class KvException : WeaveFFIException"),
        "typed exception class missing: {cs}"
    );
    assert!(
        !cs.contains("KvErrorException"),
        "doubled suffix must not appear: {cs}"
    );
    // Codes surface as PascalCase constants with their ABI values.
    assert!(
        cs.contains("public const int KeyNotFound = 1001;"),
        "code constant missing: {cs}"
    );
    assert!(
        cs.contains("public const int IoError = 1004;"),
        "code constant missing: {cs}"
    );
    // FromCode maps known codes to the typed exception and falls back to
    // the generic exception for unknown codes, with the default message
    // filling an empty slot message.
    assert!(
        cs.contains(
            "internal static WeaveFFIException FromCode(int code, string message, byte[]? payload)"
        ),
        "FromCode factory missing: {cs}"
    );
    // A code with payload fields decodes them into the exception's Data
    // dictionary in declaration order.
    assert!(
        cs.contains("case KeyNotFound:")
            && cs.contains(
                "var ex = new KvException(code, string.IsNullOrEmpty(message) ? \"Key not found\" : message);"
            ),
        "typed mapping missing: {cs}"
    );
    assert!(
        cs.contains("var reader = new WeaveFFIBufferReader(payload);")
            && cs.contains("var fKey = reader.ReadString();")
            && cs.contains("ex.Data[\"key\"] = fKey;")
            && cs.contains("var fAttempts = reader.ReadI32();")
            && cs.contains("ex.Data[\"attempts\"] = fAttempts;"),
        "payload field decode missing: {cs}"
    );
    // A code without fields maps directly.
    assert!(
        cs.contains("case IoError:")
            && cs.contains(
                "return new KvException(code, string.IsNullOrEmpty(message) ? \"I/O failure\" : message);"
            ),
        "fieldless mapping missing: {cs}"
    );
    assert!(
        cs.contains("default:") && cs.contains("return new WeaveFFIException(code, message);"),
        "generic fallback missing: {cs}"
    );
    // The error-check helper gains a per-domain variant that copies the
    // payload, clears the slot, and throws through FromCode.
    assert!(
        cs.contains("internal static void CheckKv(WeaveFFIError err)")
            && cs.contains("var payload = CopyPayload(err);")
            && cs.contains("NativeMethods.weaveffi_error_clear(ref err);")
            && cs.contains("throw KvException.FromCode(code, msg, payload);"),
        "per-domain check missing: {cs}"
    );
}

#[test]
fn interface_class_rendering() {
    let cs = render_csharp(
        &kv_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Opaque-handle wrapper following the struct pattern.
    assert!(
        cs.contains("public class Store : IDisposable"),
        "interface class missing: {cs}"
    );
    assert!(
        cs.contains("internal Store(IntPtr handle)"),
        "internal handle ctor missing: {cs}"
    );
    assert!(
        cs.contains("internal IntPtr Handle => _handle;"),
        "Handle accessor missing: {cs}"
    );
    // The `new` constructor is a real C# constructor assigning _handle.
    assert!(
        cs.contains("public Store(string path)"),
        "real constructor missing: {cs}"
    );
    assert!(
        cs.contains("_handle = result;"),
        "constructor must assign the checked handle: {cs}"
    );
    // Other constructors become static factories wrapping the pointer.
    assert!(
        cs.contains("public static Store OpenReadonly(string path)"),
        "factory missing: {cs}"
    );
    assert!(
        cs.contains("return new Store(result);"),
        "factory must wrap the owned pointer: {cs}"
    );
    // Instance method: non-static, handle as the leading argument.
    assert!(
        cs.contains("public string Get(string storeKey)"),
        "instance method missing: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_kv_Store_get(_handle, storeKeyPtr, ref err);"),
        "method must pass _handle first: {cs}"
    );
    // Static member is a plain static method.
    assert!(
        cs.contains("public static uint DefaultCapacity()"),
        "interface static missing: {cs}"
    );
    // Iterator method surfaces as IEnumerable with the handle prepended.
    assert!(
        cs.contains("public IEnumerable<string> ListKeys()"),
        "iterator method missing: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_kv_Store_list_keys(_handle, ref err);"),
        "iterator launch must pass _handle: {cs}"
    );
    // Async method returns Task and passes the handle to the launcher.
    assert!(
        cs.contains("public async Task Compact()"),
        "async method missing: {cs}"
    );
    assert!(
        cs.contains(
            "NativeMethods.weaveffi_kv_Store_compact_async(_handle, IntPtr.Zero, callback, ctx);"
        ),
        "async launch must pass _handle and the cancel slot: {cs}"
    );
    // Disposal: Dispose + finalizer calling the destroy symbol once.
    assert!(
        cs.contains("NativeMethods.weaveffi_kv_Store_destroy(_handle);") && cs.contains("~Store()"),
        "dispose/finalizer missing: {cs}"
    );
    // Externs: destroy plus shape-matched member declarations with the
    // implicit self slot on instance members.
    for sym in [
        "internal static extern void weaveffi_kv_Store_destroy(IntPtr self);",
        "internal static extern IntPtr weaveffi_kv_Store_new(IntPtr path, ref WeaveFFIError err);",
        "internal static extern IntPtr weaveffi_kv_Store_open_readonly(IntPtr path, ref WeaveFFIError err);",
        "internal static extern IntPtr weaveffi_kv_Store_get(IntPtr self, IntPtr store_key, ref WeaveFFIError err);",
        "internal static extern ulong weaveffi_kv_Store_count(IntPtr self, ref WeaveFFIError err);",
        "internal static extern IntPtr weaveffi_kv_Store_list_keys(IntPtr self, ref WeaveFFIError out_err);",
        "internal static extern int weaveffi_kv_Store_ListKeysIterator_next(",
        "internal static extern void weaveffi_kv_Store_ListKeysIterator_destroy(IntPtr iter);",
        "internal static extern void weaveffi_kv_Store_compact_async(IntPtr self, IntPtr cancel_token, AsyncCb_weaveffi_kv_Store_compact callback, IntPtr context);",
        "internal static extern uint weaveffi_kv_Store_default_capacity(ref WeaveFFIError err);",
    ] {
        assert!(cs.contains(sym), "missing P/Invoke `{sym}`: {cs}");
    }
    // No stray sync extern for the async-only member.
    assert!(
        !cs.contains("weaveffi_kv_Store_compact(IntPtr self, ref WeaveFFIError err)"),
        "async member must not declare a sync extern: {cs}"
    );
    // Interface parameters borrow the handle.
    assert!(
        cs.contains("public static ulong LookupStore(Store store)"),
        "interface param wrapper missing: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_kv_lookup_store(store.Handle, ref err);"),
        "interface param must pass obj.Handle: {cs}"
    );
}

/// Extract the body of the method whose signature contains `sig`, up to
/// the next method boundary (a blank line followed by a doc comment or
/// declaration at the same depth). Good enough to scope error-check
/// assertions to one wrapper.
fn method_slice<'a>(cs: &'a str, sig: &str) -> &'a str {
    let start = cs
        .find(sig)
        .unwrap_or_else(|| panic!("signature `{sig}` not found in: {cs}"));
    let rest = &cs[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn throws_split_typed_vs_generic() {
    let cs = render_csharp(
        &kv_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // throws == true: sync method reports through the typed check.
    let get = method_slice(&cs, "public string Get(string storeKey)");
    assert!(
        get.contains("WeaveFFIError.CheckKv(err);"),
        "throwing method must use the typed check: {get}"
    );
    // throws == false: generic check only (panics/marshalling).
    let count = method_slice(&cs, "public ulong Count()");
    assert!(
        count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckKv"),
        "non-throwing method must use the generic check: {count}"
    );
    // Free functions follow the same split.
    let lookup = method_slice(&cs, "public static ulong LookupStore(Store store)");
    assert!(
        lookup.contains("WeaveFFIError.CheckKv(err);"),
        "throwing free function must use the typed check: {lookup}"
    );
    let ping = method_slice(&cs, "public static bool Ping()");
    assert!(
        ping.contains("WeaveFFIError.Check(err);") && !ping.contains("CheckKv"),
        "non-throwing free function must use the generic check: {ping}"
    );
    // The real constructor throws the typed exception too.
    let ctor = method_slice(&cs, "public Store(string path)");
    assert!(
        ctor.contains("WeaveFFIError.CheckKv(err);"),
        "throwing constructor must use the typed check: {ctor}"
    );
    // Async completion faults the task with the typed exception; the
    // iterator's next-checks are typed as well.
    assert!(
        cs.contains("var payload = WeaveFFIError.CopyPayload(wErr);")
            && cs.contains("tcs.SetException(KvException.FromCode(wErr.Code, msg, payload));"),
        "async throws must fault with the typed exception and payload: {cs}"
    );
    let iter = method_slice(&cs, "private static IEnumerator<string> EnumerateListKeys(");
    assert!(
        iter.contains("WeaveFFIError.CheckKv(iterErr);"),
        "iterator next-check must be typed: {iter}"
    );
    // Throwing wrappers document the exception type.
    assert!(
        cs.contains(
            "/// <exception cref=\"KvException\">Thrown when the call reports a KvError code.</exception>"
        ),
        "exception doc missing: {cs}"
    );
}

#[test]
fn wrapper_params_are_camel_case() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "create_contact".into(),
            params: vec![
                Param {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: Some("Given name.".into()),
                },
                Param {
                    name: "contact_type".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        interfaces: vec![],
        modules: vec![],
    }]);
    let cs = render_csharp(
        &api,
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    assert!(
        cs.contains("public static int CreateContact(string firstName, int contactType)"),
        "wrapper params must be camelCase: {cs}"
    );
    assert!(
        cs.contains("Marshal.StringToCoTaskMemUTF8(firstName)")
            && cs.contains("Marshal.FreeCoTaskMem(firstNamePtr);"),
        "marshalling locals must follow the camelCase name: {cs}"
    );
    assert!(
        cs.contains("/// <param name=\"firstName\">Given name.</param>"),
        "param docs must use the camelCase name: {cs}"
    );
    // The P/Invoke extern keeps the IDL spelling.
    assert!(
        cs.contains("internal static extern int weaveffi_contacts_create_contact(IntPtr first_name, int contact_type, ref WeaveFFIError err);"),
        "extern must keep IDL parameter names: {cs}"
    );
}

#[test]
fn default_config_strips_module_prefix() {
    let config = DotnetConfig::default();
    assert!(
        config.strip_module_prefix,
        "strip_module_prefix must default to true"
    );
}

/// Parse, validate, and render a CLI fixture IDL end to end. Stands in
/// for the CLI-driven generation while `weaveffi-cli` is blocked on other
/// generators mid-overhaul: same parse, validation, model build, and
/// render path the CLI runs, minus the argument plumbing.
fn render_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../weaveffi-cli/tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    let api = weaveffi_ir::parse::parse_api_str(&text, "yml").expect("fixture must parse");
    let api = weaveffi_core::validate::validate_api(api, None).expect("fixture must validate");
    render_csharp(&api, "WeaveFFI", true, "weaveffi", name, "WeaveFFI.cs")
}

#[test]
fn fixture_contacts_renders_new_surface() {
    let cs = render_fixture("02_contacts.yml");
    // Interface class: real ctor for `new`, PascalCase methods with
    // camelCase parameters, disposal through the destroy symbol.
    assert!(
        cs.contains("public class ContactBook : IDisposable"),
        "ContactBook class missing: {cs}"
    );
    assert!(
        cs.contains("public ContactBook()"),
        "real constructor missing: {cs}"
    );
    assert!(
        cs.contains(
            "public Contact Add(string firstName, string lastName, string? email, \
             ContactType contactType)"
        ),
        "Add method missing: {cs}"
    );
    assert!(
        cs.contains("public Contact Get(long id)"),
        "Get method missing: {cs}"
    );
    assert!(
        cs.contains("public Contact[] List()"),
        "List method missing: {cs}"
    );
    assert!(
        cs.contains("public bool Remove(long id)"),
        "Remove method missing: {cs}"
    );
    assert!(
        cs.contains("public int Count()"),
        "Count method missing: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_contacts_ContactBook_destroy(_handle);")
            && cs.contains("~ContactBook()"),
        "dispose/finalizer missing: {cs}"
    );
    // Typed errors: domain exception with code constants, typed checks in
    // throwing methods, generic checks elsewhere.
    assert!(
        cs.contains("public class ContactsException : WeaveFFIException"),
        "ContactsException missing: {cs}"
    );
    assert!(
        cs.contains("public const int InvalidName = 1;")
            && cs.contains("public const int NotFound = 2;"),
        "code constants missing: {cs}"
    );
    let get = method_slice(&cs, "public Contact Get(long id)");
    assert!(
        get.contains("WeaveFFIError.CheckContacts(err);"),
        "throwing method must use the typed check: {get}"
    );
    let count = method_slice(&cs, "public int Count()");
    assert!(
        count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckContacts"),
        "non-throwing method must use the generic check: {count}"
    );
}

#[test]
fn fixture_inventory_renders_two_domains() {
    let cs = render_fixture("03_inventory.yml");
    // The products module owns the Catalog interface.
    assert!(
        cs.contains("public class Catalog : IDisposable"),
        "Catalog class missing: {cs}"
    );
    assert!(
        cs.contains("public Product AddProduct(string name, double price, Category category)"),
        "AddProduct method missing: {cs}"
    );
    assert!(
        cs.contains("public Product GetProduct(long id)"),
        "GetProduct method missing: {cs}"
    );
    assert!(
        cs.contains("NativeMethods.weaveffi_products_Catalog_destroy(_handle);"),
        "Catalog destroy missing: {cs}"
    );
    // Two error domains, each with its own exception and check helper.
    assert!(
        cs.contains("public class ProductsException : WeaveFFIException")
            && cs.contains("public class OrdersException : WeaveFFIException"),
        "both domain exceptions must render: {cs}"
    );
    assert!(
        cs.contains("public const int InvalidPrice = 1;")
            && cs.contains("public const int ProductNotFound = 2;")
            && cs.contains("public const int OrderNotFound = 1;")
            && cs.contains("public const int EmptyOrder = 2;"),
        "per-domain code constants missing: {cs}"
    );
    let add = method_slice(
        &cs,
        "public Product AddProduct(string name, double price, Category category)",
    );
    assert!(
        add.contains("WeaveFFIError.CheckProducts(err);"),
        "Catalog methods must use the products check: {add}"
    );
    // The orders module's free functions use their own domain.
    let create = method_slice(&cs, "public static long CreateOrder(OrderItem[] items)");
    assert!(
        create.contains("WeaveFFIError.CheckOrders(err);"),
        "orders functions must use the orders check: {create}"
    );
    let cancel = method_slice(&cs, "public static bool CancelOrder(long id)");
    assert!(
        cancel.contains("WeaveFFIError.Check(err);") && !cancel.contains("CheckOrders"),
        "non-throwing orders function must use the generic check: {cancel}"
    );
    // Per-module static classes with stripped method names.
    assert!(
        cs.contains("public static class Orders"),
        "orders wrapper class missing: {cs}"
    );
}

/// Regression: reserved-word identifiers get the `@` escape everywhere they
/// surface (wrapper params, extern params, derived locals, ctor params, and
/// field assignments). Previously only a partial keyword list was escaped,
/// so `lock`, `foreach`, or `char` produced C# that didn't compile, and a
/// ctor param named `default` silently assigned the `default` literal.
#[test]
fn keyword_identifiers_are_escaped() {
    let p = |name: &str, ty: TypeRef| Param {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
    };
    let mut m = simple_module(vec![Function {
        name: "load".into(),
        params: vec![
            p("string", TypeRef::StringUtf8),
            p("lock", TypeRef::I32),
            p("foreach", TypeRef::Bool),
            p("char", TypeRef::Bytes),
            p("normal", TypeRef::I64),
        ],
        returns: Some(TypeRef::I32),
        doc: None,
        r#async: false,
        cancellable: false,
        throws: false,
        deprecated: None,
        since: None,
    }]);
    m.structs.push(StructDef {
        name: "Config".into(),
        doc: None,
        fields: vec![
            StructField {
                name: "default".into(),
                ty: TypeRef::I32,
                doc: None,
            },
            StructField {
                name: "normal".into(),
                ty: TypeRef::Bool,
                doc: None,
            },
        ],
    });
    let cs = render_csharp(
        &make_api(vec![m]),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Wrapper signature: every keyword param carries `@`; normal ones don't.
    assert!(
        cs.contains(
            "public static int Load(string @string, int @lock, bool @foreach, byte[] @char, long normal)"
        ),
        "wrapper signature must escape keyword params: {cs}"
    );
    // The extern declaration escapes direct slots too (`char` lowers to a
    // ptr/len pair whose suffixed names aren't keywords).
    assert!(
        cs.contains("weaveffi_math_load(IntPtr @string, int @lock, byte @foreach, IntPtr char_ptr, UIntPtr char_len, long normal, ref WeaveFFIError err)"),
        "extern params must escape keywords: {cs}"
    );
    // Locals derived from an escaped param keep the escape.
    assert!(
        cs.contains("var @charPin = GCHandle.Alloc(@char, GCHandleType.Pinned);"),
        "derived pin local must stay escaped: {cs}"
    );
    // The record ctor escapes the field param and assigns the param, not
    // the C# `default` literal.
    assert!(
        cs.contains("public Config(int @default, bool normal)"),
        "ctor must escape keyword field params: {cs}"
    );
    assert!(
        cs.contains("Default = @default;"),
        "ctor must assign the escaped param, not the default literal: {cs}"
    );
}

/// Regression: the typed error mapping switches only on the declared
/// (positive) code constants and routes every other value, in particular
/// the negative runtime codes reserved by the ABI (-1 generic, -2 panic,
/// -3 marshalling), to the generic branded exception.
#[test]
fn error_mapping_reserves_negative_codes_for_runtime() {
    let cs = render_csharp(
        &kv_api(),
        "WeaveFFI",
        true,
        "weaveffi",
        "weaveffi.yml",
        "WeaveFFI.cs",
    );
    // Exactly the two declared codes appear as case arms, and both declared
    // constants are positive.
    let case_count = cs.matches("case ").count();
    assert_eq!(
        case_count, 2,
        "FromCode must switch on exactly the declared codes: {cs}"
    );
    assert!(
        cs.contains("case KeyNotFound:") && cs.contains("case IoError:"),
        "declared code arms missing: {cs}"
    );
    assert!(
        cs.contains("public const int KeyNotFound = 1001;")
            && cs.contains("public const int IoError = 1004;"),
        "declared constants must keep their positive values: {cs}"
    );
    // No literal case arm exists for any negative runtime code; they all
    // take the default arm to the generic branded exception.
    assert!(
        !cs.contains("case -"),
        "negative codes must never map to a typed case: {cs}"
    );
    assert!(
        cs.contains("default:\n                    return new WeaveFFIException(code, message);"),
        "unknown codes must fall back to WeaveFFIException: {cs}"
    );
    // Throwing paths in the domain module use the typed check; non-throwing
    // paths trap through the generic check.
    let lookup = method_slice(&cs, "public static ulong LookupStore(Store store)");
    assert!(
        lookup.contains("WeaveFFIError.CheckKv(err);"),
        "throwing function must use the typed check: {lookup}"
    );
    let ping = method_slice(&cs, "public static bool Ping()");
    assert!(
        ping.contains("WeaveFFIError.Check(err);") && !ping.contains("CheckKv"),
        "non-throwing function must trap via the generic check: {ping}"
    );
}

/// Regression: user-provided package metadata routes through `xml_escape`
/// in both manifests, so quotes, apostrophes, and ampersands can't corrupt
/// the XML. Previously the crate-local escaper missed `"` and `'`, and the
/// `.nuspec` project URL wasn't escaped at all.
#[test]
fn manifest_metadata_is_xml_escaped() {
    use weaveffi_ir::ir::Package;
    let mut api = make_api(vec![simple_module(vec![])]).api().clone();
    api.package = Some(Package {
        name: "my-kv".into(),
        version: "1.2.3".into(),
        description: Some("The \"best\" store & friends, isn't it".into()),
        license: Some("MIT OR Apache-2.0".into()),
        authors: vec!["Ada \"The First\" <ada@example.com>".into()],
        homepage: Some("https://example.com/?a=1&b=2".into()),
        repository: None,
    });
    let api = ResolvedApi::assume_resolved(api);
    let config = DotnetConfig::default();
    let package = crate::package::resolve_dotnet_package(&api, &config);
    let csproj = crate::package::render_csproj(&package, "weaveffi.yml", "WeaveFFI.csproj");
    assert!(
        csproj.contains(
            "<Description>The &quot;best&quot; store &amp; friends, isn&apos;t it</Description>"
        ),
        "csproj description must be XML-escaped: {csproj}"
    );
    assert!(
        csproj.contains("<Authors>Ada &quot;The First&quot; &lt;ada@example.com&gt;</Authors>"),
        "csproj authors must be XML-escaped: {csproj}"
    );
    assert!(
        csproj.contains("<PackageProjectUrl>https://example.com/?a=1&amp;b=2</PackageProjectUrl>"),
        "csproj project URL must be XML-escaped: {csproj}"
    );
    let nuspec = crate::package::render_nuspec(&package, "weaveffi.yml", "WeaveFFI.nuspec");
    assert!(
        nuspec.contains(
            "<description>The &quot;best&quot; store &amp; friends, isn&apos;t it</description>"
        ),
        "nuspec description must be XML-escaped: {nuspec}"
    );
    assert!(
        nuspec.contains("<projectUrl>https://example.com/?a=1&amp;b=2</projectUrl>"),
        "nuspec project URL must be XML-escaped: {nuspec}"
    );
}
