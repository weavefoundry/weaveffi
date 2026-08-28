//! Output-content tests for the Python generator: these assert on the
//! generated source text and are the byte-compatibility guard for the
//! crate.

use weaveffi_core::resolved::ResolvedApi;
use super::*;
use crate::stubs::render_pyi_module;
use crate::types::{py_ctypes_scalar, py_type_hint};
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }
}

#[test]
fn generator_name_is_python() {
    assert_eq!(Generator::name(&PythonGenerator), "python");
}

fn ping_api() -> ResolvedApi {
    make_api(vec![simple_module(vec![Function {
        name: "ping".into(),
        params: vec![],
        returns: None,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])])
}

#[test]
fn python_keywords_escape_with_trailing_underscore() {
    let mut module = simple_module(vec![Function {
        name: "import".into(),
        params: vec![Param {
            name: "class".into(),
            ty: TypeRef::I32,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::I32),
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }]);
    module.structs = vec![StructDef {
        name: "Event".into(),
        doc: None,
        fields: vec![StructField {
            name: "from".into(),
            ty: TypeRef::StringUtf8,
            doc: None,
        }],
    }];
    let api = make_api(vec![module]);
    let out = render_python_module(&api, true, "weaveffi", "test.yml");
    // A reserved function or parameter name gains a trailing underscore, and
    // the raw C call still targets the unescaped symbol.
    assert!(
        out.contains("def import_(class_: int) -> int:"),
        "escaped signature missing: {out}"
    );
    assert!(out.contains("_fn = _lib.weaveffi_math_import"), "{out}");
    assert!(
        out.contains("_result = _fn(class_, ctypes.byref(_err))"),
        "{out}"
    );
    // A reserved field name is escaped consistently in the dataclass and in
    // both codec directions.
    assert!(out.contains("    from_: str"), "{out}");
    assert!(out.contains("_w.write_string(value.from_)"), "{out}");
    assert!(out.contains("from_=_r.read_string(),"), "{out}");
    // The stub mirrors the escaped spellings.
    let model = BindingModel::build(&api, "weaveffi");
    let pyi = render_pyi_module(&model, true, "test.yml");
    assert!(
        pyi.contains("def import_(class_: int) -> int: ..."),
        "{pyi}"
    );
    assert!(pyi.contains("    from_: str"), "{pyi}");
}

#[test]
fn package_emits_per_platform_trees_and_swaps_loader() {
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = ping_api();
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(
        Platform::MacosArm64,
        "/src/darwin-arm64/libcalculator.dylib",
    );
    bins.insert(Platform::LinuxX64, "/src/linux-x64/libcalculator.so");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &PythonGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &PythonConfig::default(),
    )
    .expect("python supports packaging");

    // A complete wheel tree per bundled platform.
    assert!(files
        .iter()
        .any(|f| f.path.as_str().contains("python/darwin-arm64/")));
    assert!(files
        .iter()
        .any(|f| f.path.as_str().contains("python/linux-x64/")));
    // Exactly one bundled binary per platform, materialized as copies.
    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);

    // The loader was rewritten to prefer the bundled library (the fragile
    // string replace must keep matching the generator's loader block).
    let py = files
        .iter()
        .find(|f| {
            f.path
                .as_str()
                .ends_with("darwin-arm64/weaveffi/weaveffi.py")
        })
        .expect("weaveffi.py present");
    let FileContent::Text(src) = &py.content else {
        panic!("weaveffi.py should be text");
    };
    assert!(
        src.contains("os.path.exists") && src.contains("libcalculator.dylib"),
        "packaged loader not applied: {src}"
    );
    assert!(
        !src.contains("\"libweaveffi.dylib\""),
        "generate-mode loader leaked into the package"
    );
}

#[test]
fn generate_creates_output_files() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_output");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(
            &api,
            out_dir,
            &PythonConfig {
                strip_module_prefix: true,
                ..PythonConfig::default()
            },
        )
        .unwrap();

    let init = std::fs::read_to_string(tmp.join("python/weaveffi/__init__.py")).unwrap();
    assert!(init.contains("from .weaveffi import *"));

    let weaveffi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
    assert!(weaveffi.contains("WeaveFFI"));
    assert!(weaveffi.contains("def add("));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn output_files_lists_all() {
    let api = make_api(vec![]);
    let out = Utf8Path::new("/tmp/out");
    let files = PythonGenerator.output_files(&api, out, &PythonConfig::default());
    assert_eq!(
        files,
        vec![
            format!("{out}/python/README.md"),
            format!("{out}/python/pyproject.toml"),
            format!("{out}/python/setup.py"),
            format!("{out}/python/weaveffi/__init__.py"),
            format!("{out}/python/weaveffi/weaveffi.py"),
            format!("{out}/python/weaveffi/weaveffi.pyi"),
        ]
    );
}

#[test]
fn preamble_has_load_library() {
    let api = make_api(vec![]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("def _load_library()"), "missing _load_library");
    assert!(
        py.contains("libweaveffi.dylib"),
        "missing macOS library name"
    );
    assert!(py.contains("libweaveffi.so"), "missing Linux library name");
    assert!(py.contains("weaveffi.dll"), "missing Windows library name");
    assert!(py.contains("ctypes.CDLL(name)"), "missing CDLL call");
}

#[test]
fn preamble_has_error_handling() {
    let api = make_api(vec![]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("class WeaveFFIError(Exception):"),
        "missing error class"
    );
    assert!(
        py.contains("class _WeaveFFIErrorStruct(ctypes.Structure):"),
        "missing error struct"
    );
    assert!(py.contains("def _check_error("), "missing _check_error");
    assert!(
        py.contains("weaveffi_error_clear"),
        "missing error_clear setup"
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("def add(a: int, b: int) -> int:"),
        "missing function signature: {py}"
    );
    assert!(
        py.contains("_lib.weaveffi_math_add"),
        "missing C symbol: {py}"
    );
    assert!(
        py.contains("ctypes.c_int32, ctypes.c_int32"),
        "missing argtypes: {py}"
    );
    assert!(
        py.contains("_fn.restype = ctypes.c_int32"),
        "missing restype: {py}"
    );
    assert!(
        py.contains("_check_error(_err)"),
        "missing error check: {py}"
    );
    assert!(py.contains("return _result"), "missing return: {py}");
}

#[test]
fn string_function_encode_decode() {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("def echo(msg: str) -> str:"),
        "missing signature: {py}"
    );
    assert!(py.contains("ctypes.c_char_p"), "missing c_char_p: {py}");
    assert!(
        py.contains("_string_to_bytes(msg)"),
        "missing _string_to_bytes call: {py}"
    );
    // The owned return string is copied and released via `_take_string`.
    assert!(
        py.contains("return _take_string(_result) or \"\""),
        "missing _take_string call: {py}"
    );
}

#[test]
fn void_function() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "reset".into(),
        params: vec![],
        returns: None,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("def reset() -> None:"),
        "missing void signature: {py}"
    );
    assert!(
        py.contains("_fn.restype = None"),
        "missing None restype: {py}"
    );
    assert!(
        !py.contains("_result ="),
        "void function should not assign _result: {py}"
    );
}

#[test]
fn enum_intenum_class() {
    let api = make_api(vec![Module {
        name: "paint".into(),
        functions: vec![],
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

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("class Color(IntEnum):"),
        "missing IntEnum class: {py}"
    );
    assert!(
        py.contains("\"\"\"Primary colors\"\"\""),
        "missing doc: {py}"
    );
    assert!(py.contains("Red = 0"), "missing Red: {py}");
    assert!(py.contains("Green = 1"), "missing Green: {py}");
    assert!(py.contains("Blue = 2"), "missing Blue: {py}");
}

#[test]
fn enum_param_and_return() {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("a: \"Color\""), "missing enum param hint: {py}");
    assert!(
        py.contains("-> \"Color\":"),
        "missing enum return hint: {py}"
    );
    assert!(py.contains("a.value"), "missing .value conversion: {py}");
    assert!(
        py.contains("return Color(_result)"),
        "missing enum return wrap: {py}"
    );
}

#[test]
fn struct_dataclass_value_type() {
    let api = make_api(vec![Module {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("@dataclass\nclass Contact:"),
        "missing dataclass: {py}"
    );
    assert!(py.contains("name: str"), "missing name field: {py}");
    assert!(py.contains("age: int"), "missing age field: {py}");
    // Records are plain value types: no pointer wrapping, no destructor,
    // and no C symbols at all.
    assert!(!py.contains("self._ptr"), "no pointer wrapping: {py}");
    assert!(
        !py.contains("Contact_destroy"),
        "no destroy symbol for records: {py}"
    );
    assert!(
        !py.contains("Contact_get_"),
        "no getter symbols for records: {py}"
    );
    // The buffer codecs are generated beside the dataclass.
    assert!(
        py.contains("def _write_Contact(_w: _BufferWriter, value: \"Contact\") -> None:"),
        "missing field writer: {py}"
    );
    assert!(
        py.contains("_w.write_string(value.name)"),
        "missing string field write: {py}"
    );
    assert!(
        py.contains("_w.write_i32(value.age)"),
        "missing i32 field write: {py}"
    );
    assert!(
        py.contains("def _read_Contact(_r: _BufferReader) -> \"Contact\":"),
        "missing field reader: {py}"
    );
    assert!(
        py.contains("name=_r.read_string(),"),
        "missing string field read: {py}"
    );
    assert!(
        py.contains("age=_r.read_i32(),"),
        "missing i32 field read: {py}"
    );
    assert!(
        py.contains("def _pack_Contact(value: \"Contact\") -> bytes:"),
        "missing pack helper: {py}"
    );
    assert!(
        py.contains("def _unpack_Contact(data: bytes) -> \"Contact\":"),
        "missing unpack helper: {py}"
    );
}

#[test]
fn buffered_record_param_packs() {
    let api = ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules: vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "save_contact".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::Record("Contact".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    });
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    PythonGenerator
        .generate(&api, out, &PythonConfig::default())
        .unwrap();
    let py = std::fs::read_to_string(out.join("python/weaveffi/weaveffi.py")).unwrap();
    // The caller packs the record to bytes and passes the (ptr, len)
    // slot pair; `c_char_p` accepts the bytes directly.
    assert!(
        py.contains("def save_contact(contact: \"Contact\") -> None:"),
        "missing wrapper signature: {py}"
    );
    assert!(
        py.contains("_contact_buf = _pack_Contact(contact)"),
        "missing pack call: {py}"
    );
    assert!(
        py.contains(
            "_fn.argtypes = [ctypes.c_char_p, ctypes.c_size_t, \
             ctypes.POINTER(_WeaveFFIErrorStruct)]"
        ),
        "missing buffered argtypes: {py}"
    );
    assert!(
        py.contains("_fn(_contact_buf, len(_contact_buf), ctypes.byref(_err))"),
        "missing buffered call args: {py}"
    );
    // No builder class survives the migration.
    assert!(!py.contains("ContactBuilder"), "builders are gone: {py}");
}

#[test]
fn struct_return() {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("-> \"Contact\":"),
        "missing struct return hint: {py}"
    );
    // A buffered return keeps its raw address plus a trailing out_len.
    assert!(
        py.contains("_fn.restype = ctypes.c_void_p"),
        "missing void_p restype for buffered return: {py}"
    );
    assert!(
        py.contains("_out_len = ctypes.c_size_t(0)"),
        "missing out_len for buffered return: {py}"
    );
    // The owned encoded buffer is copied, freed, then decoded.
    assert!(
        py.contains("_data = _take_buffer(_result, _out_len.value)"),
        "missing buffer take: {py}"
    );
    assert!(
        py.contains("return _unpack_Contact(_data)"),
        "missing record decode: {py}"
    );
}

#[test]
fn bool_uses_c_int32() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("flag: bool"), "missing bool param: {py}");
    assert!(py.contains("-> bool:"), "missing bool return: {py}");
    assert!(
        py.contains("ctypes.c_int32"),
        "missing c_int32 for Bool: {py}"
    );
    assert!(
        py.contains("1 if flag else 0"),
        "missing bool-to-int conversion: {py}"
    );
    assert!(
        py.contains("return bool(_result)"),
        "missing int-to-bool conversion: {py}"
    );
}

#[test]
fn handle_uses_c_uint64() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "create".into(),
        params: vec![],
        returns: Some(TypeRef::Handle),
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("ctypes.c_uint64"),
        "missing c_uint64 for Handle: {py}"
    );
}

#[test]
fn bytes_param_and_return() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "process".into(),
            params: vec![Param {
                name: "data".into(),
                ty: TypeRef::Bytes,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Bytes),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("data: bytes"), "missing bytes param: {py}");
    assert!(py.contains("-> bytes:"), "missing bytes return: {py}");
    assert!(
        py.contains("ctypes.POINTER(ctypes.c_uint8)"),
        "missing uint8 pointer: {py}"
    );
    assert!(py.contains("ctypes.c_size_t"), "missing size_t: {py}");
    assert!(py.contains("_out_len"), "missing out_len: {py}");
}

#[test]
fn optional_value_param_and_return() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "find".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("id: Optional[int]"),
        "missing optional param: {py}"
    );
    assert!(
        py.contains("-> Optional[int]:"),
        "missing optional return: {py}"
    );
    // The optional packs into a per-parameter value buffer: a flag byte,
    // then the value when present.
    assert!(
        py.contains("_id_w = _BufferWriter()"),
        "missing param writer: {py}"
    );
    assert!(py.contains("if id is None:"), "missing None branch: {py}");
    assert!(
        py.contains("_id_w.write_option_flag(False)"),
        "missing absent flag write: {py}"
    );
    assert!(
        py.contains("_id_w.write_option_flag(True)"),
        "missing present flag write: {py}"
    );
    assert!(
        py.contains("_id_w.write_i32(id)"),
        "missing value write: {py}"
    );
    assert!(
        py.contains("_id_buf = _id_w.finish()"),
        "missing buffer finish: {py}"
    );
    assert!(
        py.contains("_fn(_id_buf, len(_id_buf), ctypes.byref(_out_len), ctypes.byref(_err))"),
        "missing buffered call args: {py}"
    );
    // The buffered return is copied, freed, and decoded through the
    // option flag.
    assert!(
        py.contains("_data = _take_buffer(_result, _out_len.value)"),
        "missing buffer take: {py}"
    );
    assert!(
        py.contains(
            "return _decode_buffer(_data, \
             lambda _r: (_r.read_i32() if _r.read_option_flag() else None))"
        ),
        "missing optional decode: {py}"
    );
}

#[test]
fn optional_string_return() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "get_name".into(),
            params: vec![],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("-> Optional[str]:"),
        "missing optional str return: {py}"
    );
    // An optional string is buffered: the encoded value is copied,
    // freed, and decoded through the option flag.
    assert!(
        py.contains("_data = _take_buffer(_result, _out_len.value)"),
        "missing buffer take for optional string: {py}"
    );
    assert!(
        py.contains(
            "return _decode_buffer(_data, \
             lambda _r: (_r.read_string() if _r.read_option_flag() else None))"
        ),
        "missing optional string decode: {py}"
    );
}

#[test]
fn list_param_and_return() {
    let api = make_api(vec![Module {
        name: "batch".into(),
        functions: vec![
            Function {
                name: "process".into(),
                params: vec![Param {
                    name: "ids".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_ids".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("ids: List[int]"), "missing list param: {py}");
    assert!(py.contains("-> List[int]:"), "missing list return: {py}");
    // The list packs into a value buffer: a count, then each element.
    assert!(
        py.contains("_ids_w.write_len(len(ids))"),
        "missing list length write: {py}"
    );
    assert!(py.contains("for _e0 in ids:"), "missing element loop: {py}");
    assert!(
        py.contains("_ids_w.write_i32(_e0)"),
        "missing element write: {py}"
    );
    assert!(
        py.contains("_out_len"),
        "missing out_len for list return: {py}"
    );
    assert!(
        py.contains("[_r.read_i32() for _i0 in range(_r.read_len())]"),
        "missing list decode: {py}"
    );
}

#[test]
fn map_param_and_return() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![
            Function {
                name: "update".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_scores".into(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("scores: Dict[str, int]"),
        "missing map param: {py}"
    );
    assert!(
        py.contains("-> Dict[str, int]:"),
        "missing map return: {py}"
    );
    // The map packs into a value buffer: a count, then alternating
    // key and value.
    assert!(
        py.contains("_scores_w.write_len(len(scores))"),
        "missing map length write: {py}"
    );
    assert!(
        py.contains("for _k0, _v0 in scores.items():"),
        "missing entry loop: {py}"
    );
    assert!(
        py.contains("_scores_w.write_string(_k0)"),
        "missing key write: {py}"
    );
    assert!(
        py.contains("_scores_w.write_i32(_v0)"),
        "missing value write: {py}"
    );
    assert!(
        py.contains("dict((_r.read_string(), _r.read_i32()) for _i0 in range(_r.read_len()))"),
        "missing map decode: {py}"
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("email: Optional[str]"),
        "missing optional field annotation: {py}"
    );
    // The codec writes a flag byte, then the value only when present.
    assert!(
        py.contains("if value.email is None:"),
        "missing None branch in writer: {py}"
    );
    assert!(
        py.contains("_w.write_option_flag(False)"),
        "missing absent flag write: {py}"
    );
    assert!(
        py.contains("_w.write_string(value.email)"),
        "missing present value write: {py}"
    );
    assert!(
        py.contains("email=(_r.read_string() if _r.read_option_flag() else None),"),
        "missing optional field read: {py}"
    );
}

#[test]
fn struct_enum_field() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "role".into(),
                ty: TypeRef::Enum("Role".into()),
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("role: \"Role\""),
        "missing enum field annotation: {py}"
    );
    // C-style enum fields serialize as their i32 discriminant.
    assert!(
        py.contains("_w.write_i32(value.role)"),
        "missing enum field write: {py}"
    );
    assert!(
        py.contains("role=Role(_r.read_i32()),"),
        "missing enum field read: {py}"
    );
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
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "list_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "count_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_contacts");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(
            &api,
            out_dir,
            &PythonConfig {
                strip_module_prefix: true,
                ..PythonConfig::default()
            },
        )
        .unwrap();

    let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();

    assert!(py.contains("class ContactType(IntEnum):"));
    assert!(py.contains("Personal = 0"));
    assert!(py.contains("Work = 1"));

    assert!(py.contains("@dataclass\nclass Contact:"));
    assert!(py.contains("id: int"));
    assert!(py.contains("first_name: str"));
    assert!(py.contains("email: Optional[str]"));
    assert!(py.contains("contact_type: \"ContactType\""));
    // Records carry no C symbols; only the buffer codecs exist.
    assert!(!py.contains("Contact_destroy"));
    assert!(!py.contains("Contact_get_"));
    assert!(py.contains("def _pack_Contact("));
    assert!(py.contains("def _unpack_Contact("));

    assert!(py.contains("def create_contact("));
    assert!(py.contains("weaveffi_contacts_create_contact"));
    assert!(py.contains("def get_contact(id: int) -> \"Contact\":"));
    assert!(py.contains("def list_contacts() -> List[\"Contact\"]:"));
    assert!(py.contains("def count_contacts() -> int:"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn type_hint_mapping() {
    assert_eq!(py_type_hint(&TypeRef::I32), "int");
    assert_eq!(py_type_hint(&TypeRef::U32), "int");
    assert_eq!(py_type_hint(&TypeRef::I64), "int");
    assert_eq!(py_type_hint(&TypeRef::F64), "float");
    assert_eq!(py_type_hint(&TypeRef::Bool), "bool");
    assert_eq!(py_type_hint(&TypeRef::StringUtf8), "str");
    assert_eq!(py_type_hint(&TypeRef::Bytes), "bytes");
    assert_eq!(py_type_hint(&TypeRef::Handle), "int");
    assert_eq!(py_type_hint(&TypeRef::Record("Foo".into())), "\"Foo\"");
    assert_eq!(py_type_hint(&TypeRef::Enum("Bar".into())), "\"Bar\"");
    // A typed handle is an opaque pointer-sized token surfacing as a
    // plain int, matching the untyped handle.
    assert_eq!(py_type_hint(&TypeRef::TypedHandle("Foo".into())), "int");
    assert_eq!(
        py_type_hint(&TypeRef::TypedHandle("kv.Store".into())),
        "int"
    );
    // Cross-module references (resolved to a qualified IR name) must still
    // annotate the bare *local* class, which is the only symbol that exists
    // in the generated module.
    assert_eq!(
        py_type_hint(&TypeRef::Record("kv.Store".into())),
        "\"Store\""
    );
    assert_eq!(py_type_hint(&TypeRef::Enum("kv.Kind".into())), "\"Kind\"");
    assert_eq!(
        py_type_hint(&TypeRef::Optional(Box::new(TypeRef::I32))),
        "Optional[int]"
    );
    assert_eq!(
        py_type_hint(&TypeRef::List(Box::new(TypeRef::I32))),
        "List[int]"
    );
    assert_eq!(
        py_type_hint(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )),
        "Dict[str, int]"
    );
}

#[test]
fn ctypes_scalar_mapping() {
    assert_eq!(py_ctypes_scalar(&TypeRef::I32), "ctypes.c_int32");
    assert_eq!(py_ctypes_scalar(&TypeRef::U32), "ctypes.c_uint32");
    assert_eq!(py_ctypes_scalar(&TypeRef::I64), "ctypes.c_int64");
    assert_eq!(py_ctypes_scalar(&TypeRef::F64), "ctypes.c_double");
    assert_eq!(py_ctypes_scalar(&TypeRef::Bool), "ctypes.c_int32");
    assert_eq!(py_ctypes_scalar(&TypeRef::StringUtf8), "ctypes.c_char_p");
    assert_eq!(py_ctypes_scalar(&TypeRef::Handle), "ctypes.c_uint64");
    assert_eq!(py_ctypes_scalar(&TypeRef::Bytes), "ctypes.c_uint8");
    assert_eq!(
        py_ctypes_scalar(&TypeRef::TypedHandle("X".into())),
        "ctypes.c_void_p"
    );
    assert_eq!(
        py_ctypes_scalar(&TypeRef::Enum("X".into())),
        "ctypes.c_int32"
    );
}

#[test]
fn list_struct_return() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![Function {
            name: "list_items".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Record("Item".into())))),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("-> List[\"Item\"]:"),
        "missing list struct return: {py}"
    );
    // Each element decodes through the record's reader.
    assert!(
        py.contains("[_read_Item(_r) for _i0 in range(_r.read_len())]"),
        "missing record list decode: {py}"
    );
}

#[test]
fn struct_bytes_field() {
    let api = make_api(vec![Module {
        name: "storage".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Blob".into(),
            doc: None,
            fields: vec![StructField {
                name: "data".into(),
                ty: TypeRef::Bytes,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("data: bytes"),
        "missing bytes field annotation: {py}"
    );
    assert!(
        py.contains("_w.write_bytes(value.data)"),
        "missing bytes field write: {py}"
    );
    assert!(
        py.contains("data=_r.read_bytes(),"),
        "missing bytes field read: {py}"
    );
}

#[test]
fn python_generates_type_stubs() {
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
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "tags".into(),
                    ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "metadata".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    doc: None,
                },
            ],
        }],
        functions: vec![
            Function {
                name: "create_contact".into(),
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
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_python_pyi");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(
            &api,
            out_dir,
            &PythonConfig {
                strip_module_prefix: true,
                ..PythonConfig::default()
            },
        )
        .unwrap();

    let pyi_path = tmp.join("python/weaveffi/weaveffi.pyi");
    assert!(pyi_path.exists(), ".pyi file must exist");

    let pyi = std::fs::read_to_string(&pyi_path).unwrap();

    assert!(
        pyi.contains("from enum import IntEnum"),
        "missing IntEnum import"
    );
    assert!(
        pyi.contains("from typing import Callable, Dict, Iterator, List, Optional"),
        "missing typing imports"
    );

    assert!(
        pyi.contains("class ContactType(IntEnum):"),
        "missing enum stub"
    );
    assert!(
        pyi.contains("    Personal: int"),
        "missing enum variant Personal"
    );
    assert!(pyi.contains("    Work: int"), "missing enum variant Work");

    assert!(pyi.contains("class Contact:"), "missing struct stub");
    assert!(pyi.contains("    id: int\n"), "missing id field: {pyi}");
    assert!(pyi.contains("    name: str\n"), "missing name field: {pyi}");
    assert!(
        pyi.contains("    email: Optional[str]\n"),
        "missing email field: {pyi}"
    );
    assert!(
        pyi.contains("    tags: List[str]\n"),
        "missing tags field: {pyi}"
    );
    assert!(
        pyi.contains("    metadata: Dict[str, int]\n"),
        "missing metadata field: {pyi}"
    );
    assert!(
        pyi.contains(
            "    def __init__(self, id: int, name: str, email: Optional[str], \
             tags: List[str], metadata: Dict[str, int]) -> None: ..."
        ),
        "missing dataclass constructor stub: {pyi}"
    );

    assert!(
        pyi.contains("def create_contact(name: str, email: Optional[str]) -> int: ..."),
        "missing create_contact stub: {pyi}"
    );
    assert!(
        pyi.contains("def get_contact(id: int) -> \"Contact\": ..."),
        "missing get_contact stub: {pyi}"
    );
    assert!(
        pyi.contains("def delete_contact(id: int) -> None: ..."),
        "missing delete_contact stub: {pyi}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_python_basic() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_py_basic");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(
            &api,
            out_dir,
            &PythonConfig {
                strip_module_prefix: true,
                ..PythonConfig::default()
            },
        )
        .unwrap();

    let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();

    assert!(py.contains("def add(a: int, b: int) -> int:"));
    assert!(py.contains("_fn = _lib.weaveffi_math_add"));
    assert!(py.contains("ctypes.c_int32, ctypes.c_int32"));
    assert!(py.contains("_fn.restype = ctypes.c_int32"));
    assert!(py.contains("_err = _WeaveFFIErrorStruct()"));
    assert!(py.contains("_check_error(_err)"));
    assert!(py.contains("return _result"));

    assert!(py.contains("import ctypes"));
    assert!(py.contains("from enum import IntEnum"));
    assert!(py.contains("from typing import Callable, Dict, Iterator, List, Optional"));
    assert!(py.contains("class WeaveFFIError(Exception):"));
    assert!(py.contains("def _load_library()"));
    assert!(py.contains("_lib = _load_library()"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_python_with_structs() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: Some("A contact record".into()),
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
                    name: "last_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("@dataclass\nclass Contact:"),
        "missing dataclass decl"
    );
    assert!(
        py.contains("\"\"\"A contact record\"\"\""),
        "missing doc: {py}"
    );
    // Value type: no pointer wrapping, no destructor, no getters.
    assert!(!py.contains("self._ptr = _ptr"));
    assert!(!py.contains("weaveffi_contacts_Contact_destroy"));
    assert!(!py.contains("weaveffi_contacts_Contact_get_id"));

    assert!(py.contains("    id: int"));
    assert!(py.contains("    first_name: str"));
    assert!(py.contains("    last_name: str"));
    assert!(py.contains("    email: Optional[str]"));

    // The codec functions serialize fields in declaration order.
    assert!(py.contains("_w.write_i64(value.id)"));
    assert!(py.contains("_w.write_string(value.first_name)"));
    assert!(py.contains("id=_r.read_i64(),"));
    assert!(py.contains("email=(_r.read_string() if _r.read_option_flag() else None),"));
}

#[test]
fn generate_python_with_enums() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "get_type".into(),
            params: vec![Param {
                name: "ct".into(),
                ty: TypeRef::Enum("ContactType".into()),
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Enum("ContactType".into())),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
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

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(py.contains("class ContactType(IntEnum):"));
    assert!(py.contains("\"\"\"Type of contact\"\"\""));
    assert!(py.contains("Personal = 0"));
    assert!(py.contains("Work = 1"));
    assert!(py.contains("Other = 2"));

    assert!(
        py.contains("ct: \"ContactType\""),
        "missing enum param hint"
    );
    assert!(
        py.contains("-> \"ContactType\":"),
        "missing enum return hint"
    );
    assert!(py.contains("ct.value"), "missing .value for enum param");
    assert!(
        py.contains("return ContactType(_result)"),
        "missing enum return wrap"
    );
    assert!(py.contains("ctypes.c_int32"), "enum should use c_int32 ABI");
}

#[test]
fn generate_python_with_optionals() {
    let api = make_api(vec![Module {
        name: "store".into(),
        functions: vec![
            Function {
                name: "find_int".into(),
                params: vec![Param {
                    name: "key".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "find_name".into(),
                params: vec![Param {
                    name: "prefix".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "find_contact".into(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "find_flag".into(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Bool))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("key: Optional[int]"),
        "missing Optional[int] param"
    );
    assert!(
        py.contains("-> Optional[int]:"),
        "missing Optional[int] return"
    );
    // Optional scalars pack into per-parameter value buffers.
    assert!(
        py.contains("_key_w.write_option_flag(False)"),
        "missing absent flag write for key"
    );
    assert!(
        py.contains("_key_w.write_i32(key)"),
        "missing present value write for key"
    );

    assert!(
        py.contains("prefix: Optional[str]"),
        "missing Optional[str] param"
    );
    assert!(
        py.contains("-> Optional[str]:"),
        "missing Optional[str] return"
    );
    assert!(
        py.contains("_prefix_w.write_string(prefix)"),
        "missing optional string write"
    );

    assert!(
        py.contains("-> Optional[\"Contact\"]:"),
        "missing Optional struct return"
    );
    assert!(
        py.contains("lambda _r: (_read_Contact(_r) if _r.read_option_flag() else None)"),
        "missing optional struct decode"
    );

    assert!(
        py.contains("-> Optional[bool]:"),
        "missing Optional[bool] return"
    );
    assert!(
        py.contains("lambda _r: (_r.read_bool() if _r.read_option_flag() else None)"),
        "missing optional bool decode"
    );
}

#[test]
fn generate_python_with_lists() {
    let api = make_api(vec![Module {
        name: "batch".into(),
        functions: vec![
            Function {
                name: "process_ids".into(),
                params: vec![Param {
                    name: "ids".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_names".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_items".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Item".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(py.contains("ids: List[int]"), "missing List[int] param");
    // The list packs into a value buffer passed as a (ptr, len) pair.
    assert!(
        py.contains("_ids_w.write_len(len(ids))"),
        "missing list length write"
    );
    assert!(
        py.contains("_ids_w.write_i32(_e0)"),
        "missing list element write"
    );
    assert!(py.contains("ctypes.c_size_t"), "missing size_t for length");

    assert!(
        py.contains("-> List[str]:"),
        "missing List[str] return: {py}"
    );
    assert!(
        py.contains("[_r.read_string() for _i0 in range(_r.read_len())]"),
        "missing string list decode: {py}"
    );

    assert!(
        py.contains("-> List[\"Item\"]:"),
        "missing List struct return"
    );
    assert!(
        py.contains("[_read_Item(_r) for _i0 in range(_r.read_len())]"),
        "missing record list decode"
    );
}

#[test]
fn generate_python_with_maps() {
    let api = make_api(vec![Module {
        name: "config".into(),
        functions: vec![
            Function {
                name: "set_config".into(),
                params: vec![Param {
                    name: "settings".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "get_config".into(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("settings: Dict[str, int]"),
        "missing Dict param hint"
    );
    // The map packs into a single value buffer: count, then alternating
    // key and value.
    assert!(
        py.contains("_settings_w.write_len(len(settings))"),
        "missing map length write"
    );
    assert!(
        py.contains("for _k0, _v0 in settings.items():"),
        "missing entry loop"
    );
    assert!(
        py.contains("_settings_w.write_string(_k0)"),
        "missing key write"
    );
    assert!(
        py.contains("_settings_w.write_i32(_v0)"),
        "missing value write"
    );
    assert!(
        py.contains("_fn(_settings_buf, len(_settings_buf), ctypes.byref(_err))"),
        "missing buffered call args"
    );

    assert!(
        py.contains("-> Dict[str, int]:"),
        "missing Dict return hint"
    );
    assert!(
        py.contains("_out_len = ctypes.c_size_t(0)"),
        "missing out_len init"
    );
    // The returned buffer is copied, freed, then decoded into a dict.
    assert!(
        py.contains("_data = _take_buffer(_result, _out_len.value)"),
        "missing buffer take"
    );
    assert!(
        py.contains("dict((_r.read_string(), _r.read_i32()) for _i0 in range(_r.read_len()))"),
        "missing map decode"
    );
}

#[test]
fn generate_python_pyi_types() {
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
                    name: "tags".into(),
                    ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                },
                StructField {
                    name: "scores".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    doc: None,
                },
            ],
        }],
        functions: vec![
            Function {
                name: "create_contact".into(),
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
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "list_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");

    assert!(pyi.contains("from enum import IntEnum"));
    assert!(pyi.contains("from typing import Callable, Dict, Iterator, List, Optional"));

    assert!(pyi.contains("class ContactType(IntEnum):"));
    assert!(pyi.contains("    Personal: int"));
    assert!(pyi.contains("    Work: int"));
    assert!(pyi.contains("    Other: int"));

    assert!(pyi.contains("class Contact:"));
    assert!(pyi.contains("    id: int"));
    assert!(pyi.contains("    first_name: str"));
    assert!(pyi.contains("    email: Optional[str]"));
    assert!(pyi.contains("    tags: List[str]"));
    assert!(pyi.contains("    scores: Dict[str, int]"));
    assert!(pyi.contains(
        "    def __init__(self, id: int, first_name: str, email: Optional[str], \
         tags: List[str], scores: Dict[str, int]) -> None: ..."
    ));

    assert!(pyi.contains("def create_contact(name: str, email: Optional[str]) -> int: ..."));
    assert!(pyi.contains("def get_contact(id: int) -> \"Contact\": ..."));
    assert!(pyi.contains("def list_contacts() -> List[\"Contact\"]: ..."));
    assert!(pyi.contains("def delete_contact(id: int) -> None: ..."));
}

#[test]
fn generate_python_full_contacts() {
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
                throws: false,
                r#async: false,
                cancellable: false,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "list_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
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
                returns: Some(TypeRef::Bool),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "count_contacts".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = std::env::temp_dir().join("weaveffi_test_py_full_contacts");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(
            &api,
            out_dir,
            &PythonConfig {
                strip_module_prefix: true,
                ..PythonConfig::default()
            },
        )
        .unwrap();

    let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
    let pyi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.pyi")).unwrap();

    assert!(py.contains("class ContactType(IntEnum):"));
    assert!(py.contains("Personal = 0"));
    assert!(py.contains("Work = 1"));
    assert!(py.contains("Other = 2"));

    assert!(py.contains("@dataclass\nclass Contact:"));
    assert!(!py.contains("weaveffi_contacts_Contact_destroy"));
    assert!(!py.contains("weaveffi_contacts_Contact_get_id"));
    assert!(py.contains("    id: int"));
    assert!(py.contains("    first_name: str"));
    assert!(py.contains("    last_name: str"));
    assert!(py.contains("    email: Optional[str]"));
    assert!(py.contains("    contact_type: \"ContactType\""));
    assert!(py.contains("def _pack_Contact("));
    assert!(py.contains("def _unpack_Contact("));
    assert!(py.contains("contact_type=ContactType(_r.read_i32()),"));

    assert!(py.contains("def create_contact("));
    assert!(py.contains("first_name: str"));
    assert!(py.contains("last_name: str"));
    assert!(py.contains("email: Optional[str]"));
    assert!(py.contains("contact_type: \"ContactType\""));
    assert!(py.contains("-> int:"));
    assert!(py.contains("weaveffi_contacts_create_contact"));
    assert!(py.contains("_string_to_bytes(first_name)"));
    assert!(py.contains("contact_type.value"));

    assert!(py.contains("def get_contact(id: int) -> \"Contact\":"));
    assert!(py.contains("weaveffi_contacts_get_contact"));
    assert!(py.contains("return _unpack_Contact(_data)"));

    assert!(py.contains("def list_contacts() -> List[\"Contact\"]:"));
    assert!(py.contains("weaveffi_contacts_list_contacts"));
    assert!(py.contains("[_read_Contact(_r) for _i0 in range(_r.read_len())]"));

    assert!(py.contains("def delete_contact(id: int) -> bool:"));
    assert!(py.contains("weaveffi_contacts_delete_contact"));
    assert!(py.contains("return bool(_result)"));

    assert!(py.contains("def count_contacts() -> int:"));
    assert!(py.contains("weaveffi_contacts_count_contacts"));

    assert!(pyi.contains("class ContactType(IntEnum):"));
    assert!(pyi.contains("    Personal: int"));
    assert!(pyi.contains("    Work: int"));
    assert!(pyi.contains("    Other: int"));
    assert!(pyi.contains("class Contact:"));
    assert!(pyi.contains("def create_contact("));
    assert!(pyi.contains("def get_contact(id: int) -> \"Contact\": ..."));
    assert!(pyi.contains("def list_contacts() -> List[\"Contact\"]: ..."));
    assert!(pyi.contains("def delete_contact(id: int) -> bool: ..."));
    assert!(pyi.contains("def count_contacts() -> int: ..."));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn python_generates_packaging() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let tmp = std::env::temp_dir().join("weaveffi_test_python_packaging");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator
        .generate(&api, out_dir, &PythonConfig::default())
        .unwrap();

    let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
    assert!(
        pyproject.contains("[build-system]"),
        "missing build-system: {pyproject}"
    );
    assert!(
        pyproject.contains("setuptools"),
        "missing setuptools: {pyproject}"
    );
    assert!(
        pyproject.contains("[project]"),
        "missing project section: {pyproject}"
    );
    assert!(
        pyproject.contains("name = \"weaveffi\""),
        "missing project name: {pyproject}"
    );
    assert!(
        pyproject.contains("version = \"0.1.0\""),
        "missing version: {pyproject}"
    );
    assert!(
        pyproject.contains("[tool.setuptools]"),
        "missing tool.setuptools: {pyproject}"
    );
    assert!(
        pyproject.contains("packages = [\"weaveffi\"]"),
        "missing packages list: {pyproject}"
    );

    let setup = std::fs::read_to_string(tmp.join("python/setup.py")).unwrap();
    assert!(
        setup.contains("from setuptools import setup"),
        "missing setuptools import: {setup}"
    );
    assert!(
        setup.contains("name=\"weaveffi\""),
        "missing package name: {setup}"
    );

    let readme = std::fs::read_to_string(tmp.join("python/README.md")).unwrap();
    assert!(
        readme.contains("pip install"),
        "missing install instructions: {readme}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn python_has_memory_helpers() {
    let api = make_api(vec![]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("import contextlib"),
        "missing contextlib import"
    );
    assert!(
        py.contains("class _PointerGuard(contextlib.AbstractContextManager):"),
        "missing _PointerGuard class"
    );
    assert!(
        py.contains("def __exit__(self, *exc)"),
        "missing _PointerGuard.__exit__"
    );
    assert!(
        py.contains("def _string_to_bytes("),
        "missing _string_to_bytes helper"
    );
    assert!(
        py.contains("def _bytes_to_string("),
        "missing _bytes_to_string helper"
    );
}

#[test]
fn python_custom_package_name() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let config = PythonConfig {
        package_name: Some("my_bindings".into()),
        ..PythonConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_py_custom_pkg");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator.generate(&api, out_dir, &config).unwrap();

    assert!(
        tmp.join("python/my_bindings/__init__.py").exists(),
        "package dir should use custom name"
    );
    assert!(
        tmp.join("python/my_bindings/weaveffi.py").exists(),
        "module file should be inside custom package dir"
    );

    let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
    assert!(
        pyproject.contains("name = \"my_bindings\""),
        "pyproject.toml should use custom name: {pyproject}"
    );
    assert!(
        pyproject.contains("packages = [\"my_bindings\"]"),
        "pyproject.toml packages should use custom name: {pyproject}"
    );

    let setup = std::fs::read_to_string(tmp.join("python/setup.py")).unwrap();
    assert!(
        setup.contains("name=\"my_bindings\""),
        "setup.py should use custom name: {setup}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn python_strip_module_prefix() {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let config = PythonConfig {
        strip_module_prefix: true,
        ..PythonConfig::default()
    };

    let tmp = std::env::temp_dir().join("weaveffi_test_python_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    PythonGenerator.generate(&api, out_dir, &config).unwrap();

    let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
    assert!(
        py.contains("def create_contact("),
        "stripped name should be create_contact: {py}"
    );
    assert!(
        !py.contains("def contacts_create_contact("),
        "should not contain module-prefixed name: {py}"
    );
    assert!(
        py.contains("weaveffi_contacts_create_contact"),
        "C ABI call should still use full name: {py}"
    );

    let pyi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.pyi")).unwrap();
    assert!(
        pyi.contains("def create_contact("),
        "pyi stripped name should be create_contact: {pyi}"
    );

    // Stripping is the default; module-prefixed names are opt-in.
    assert!(PythonConfig::default().strip_module_prefix);

    let no_strip = PythonConfig {
        strip_module_prefix: false,
        ..PythonConfig::default()
    };
    let tmp2 = std::env::temp_dir().join("weaveffi_test_python_no_strip_prefix");
    let _ = std::fs::remove_dir_all(&tmp2);
    std::fs::create_dir_all(&tmp2).unwrap();
    let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

    PythonGenerator.generate(&api, out_dir2, &no_strip).unwrap();

    let py2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.py")).unwrap();
    assert!(
        py2.contains("def contacts_create_contact("),
        "opting out should use module-prefixed name: {py2}"
    );

    let pyi2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.pyi")).unwrap();
    assert!(
        pyi2.contains("def contacts_create_contact("),
        "pyi opt-out should use module-prefixed name: {pyi2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&tmp2);
}

#[test]
fn python_deeply_nested_optional() {
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
            throws: false,
            r#async: false,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        pyi.contains("Optional[List[Optional["),
        "should contain deeply nested optional type: {pyi}"
    );
}

#[test]
fn python_map_of_lists() {
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        pyi.contains("Dict[str, List[int]]"),
        "should contain map of lists type: {pyi}"
    );
}

#[test]
fn python_enum_keyed_map() {
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
            throws: false,
            r#async: false,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        pyi.contains("Dict[\"Color\", \"Contact\"]"),
        "should contain enum-keyed map type: {pyi}"
    );
}

#[test]
fn python_typed_handle_type() {
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
                throws: false,
                r#async: false,
                cancellable: false,
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    });
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    // A typed handle is an opaque token: hinted as int, passed raw.
    assert!(
        py.contains("def get_info(contact: int) -> None:"),
        "TypedHandle should hint as int: {py}"
    );
    assert!(
        !py.contains("contact._ptr"),
        "TypedHandle call arg must pass the raw value: {py}"
    );
    assert!(
        py.contains("_fn(contact, ctypes.byref(_err))"),
        "TypedHandle should pass through unchanged: {py}"
    );
    assert!(
        py.contains("ctypes.c_void_p"),
        "TypedHandle ctypes type should be c_void_p: {py}"
    );
}

#[test]
fn python_no_double_free_on_error() {
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
            throws: false,
            r#async: false,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("_string_to_bytes(name)"),
        "string param should use _string_to_bytes(name): {py}"
    );
    assert!(
        !py.contains("weaveffi_free_string(name"),
        "input string param must not be freed with weaveffi_free_string(name): {py}"
    );
    assert!(
        !py.contains("free(name"),
        "input string param must not be passed to free(name: {py}"
    );

    let fn_sig = "def find_contact(name: str) -> \"Contact\":";
    let start = py
        .find(fn_sig)
        .unwrap_or_else(|| panic!("missing find_contact signature: {py}"));
    let rest = &py[start..];
    let end_offset = rest[1..]
        .find("\n\ndef ")
        .or_else(|| rest[1..].find("\n\nclass "))
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end_offset];
    // The error check runs before the returned buffer is touched, so a
    // trap never leaves a dangling decode.
    let err_pos = body
        .find("_check_error(_err)")
        .expect("_check_error should appear in find_contact");
    let take_pos = body
        .find("_data = _take_buffer(_result, _out_len.value)")
        .expect("buffer take should appear in find_contact");
    let contact_pos = body
        .find("return _unpack_Contact(_data)")
        .expect("return _unpack_Contact(_data) should appear in find_contact");
    assert!(
        err_pos < take_pos && take_pos < contact_pos,
        "_check_error(_err) should precede the buffer take and decode: {body}"
    );

    // Records are value types now: no destructor exists to double-free.
    let class_start = py
        .find("class Contact:")
        .expect("Contact class should be defined");
    let after_class = &py[class_start..];
    let class_end = after_class[1..]
        .find("\n\nclass ")
        .or_else(|| after_class[1..].find("\n\ndef "))
        .map(|i| i + 1)
        .unwrap_or(after_class.len());
    let contact_class = &after_class[..class_end];
    assert!(
        !contact_class.contains("def __del__(self)"),
        "Contact must not define __del__: {contact_class}"
    );
    assert!(
        !contact_class.contains("_destroy"),
        "Contact must not reference a destroy symbol: {contact_class}"
    );
}

#[test]
fn python_null_check_on_optional_return() {
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
            throws: false,
            r#async: false,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);

    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // An optional record is buffered: the None case rides the option
    // flag inside the value buffer rather than a null pointer check.
    assert!(
        py.contains("_data = _take_buffer(_result, _out_len.value)"),
        "optional struct return should take the buffer: {py}"
    );
    assert!(
        py.contains(
            "return _decode_buffer(_data, \
             lambda _r: (_read_Contact(_r) if _r.read_option_flag() else None))"
        ),
        "optional struct return should decode through the flag: {py}"
    );
}

#[test]
fn python_async_function_is_async_def() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "fetch_data".into(),
        params: vec![Param {
            name: "id".into(),
            ty: TypeRef::I32,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::StringUtf8),
        doc: None,
        throws: false,
        r#async: true,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);
    let code = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        code.contains("import asyncio"),
        "should import asyncio: {code}"
    );
    assert!(
        code.contains("async def fetch_data(id: int) -> str:"),
        "should have async wrapper: {code}"
    );
    // Callback-driven: the wrapper awaits a future resolved by the C
    // completion callback, rather than blocking an executor thread.
    assert!(
        code.contains("_loop = asyncio.get_running_loop()"),
        "should use get_running_loop: {code}"
    );
    assert!(
        code.contains("_fut = _loop.create_future()"),
        "should create a future: {code}"
    );
    // The string result slot is `c_void_p` (a `c_char_p` slot would
    // auto-convert to `bytes` and lose the pointer `_take_string` frees).
    assert!(
        code.contains(
            "_cb_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p, \
                       ctypes.POINTER(_WeaveFFIErrorStruct), ctypes.c_void_p)"
        ),
        "should build the CFUNCTYPE trampoline: {code}"
    );
    assert!(
        code.contains("_loop.call_soon_threadsafe(_resolve)"),
        "should resolve via call_soon_threadsafe: {code}"
    );
    assert!(
        code.contains("return await _fut"),
        "should await the future: {code}"
    );
    assert!(
        !code.contains("run_in_executor"),
        "executor-based async must be gone: {code}"
    );
    assert!(
        !code.contains("_fetch_data_sync"),
        "sync helper must be gone: {code}"
    );
}

#[test]
fn listener_register_unregister_wrappers() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        callbacks: vec![CallbackDef {
            name: "OnMessage".into(),
            params: vec![Param {
                name: "message".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        ..simple_module(vec![])
    }]);
    let code = render_python_module(&api, false, "weaveffi", "weaveffi.yml");
    // CFUNCTYPE alias matches the C typedef shape: (const char*, void*).
    assert!(
        code.contains(
            "_CFUNC_weaveffi_math_OnMessage_fn = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_void_p)"
        ),
        "callback CFUNCTYPE alias: {code}"
    );
    // Registry pinning keeps the trampoline alive until unregister.
    assert!(
        code.contains("_listener_refs: Dict[int, object] = {}"),
        "listener registry: {code}"
    );
    assert!(
        code.contains(
            "def math_register_message_listener(callback: Callable[[str], None]) -> int:"
        ),
        "register wrapper: {code}"
    );
    assert!(
        code.contains("callback(_bytes_to_string(message))"),
        "trampoline converts the C string: {code}"
    );
    assert!(
        code.contains("_listener_refs[_listener_id] = _cfunc"),
        "register pins the trampoline: {code}"
    );
    assert!(
        code.contains("def math_unregister_message_listener(listener_id: int) -> None:"),
        "unregister wrapper: {code}"
    );
    assert!(
        code.contains("_listener_refs.pop(listener_id, None)"),
        "unregister releases the trampoline: {code}"
    );
}

#[test]
fn listener_bytes_and_enum_params_convert() {
    use weaveffi_ir::ir::{CallbackDef, EnumDef, EnumVariant, ListenerDef};
    let api = make_api(vec![Module {
        enums: vec![EnumDef {
            name: "Level".into(),
            doc: None,
            variants: vec![EnumVariant {
                name: "Info".into(),
                value: 0,
                doc: None,
                fields: vec![],
            }],
        }],
        callbacks: vec![CallbackDef {
            name: "OnChunk".into(),
            params: vec![
                Param {
                    name: "data".into(),
                    ty: TypeRef::Bytes,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "level".into(),
                    ty: TypeRef::Enum("Level".into()),
                    mutable: false,
                    doc: None,
                },
            ],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "chunks".into(),
            event_callback: "OnChunk".into(),
            doc: None,
        }],
        ..simple_module(vec![])
    }]);
    let code = render_python_module(&api, false, "weaveffi", "weaveffi.yml");
    // Bytes lower to (ptr, len) slots; the trampoline reconstructs bytes.
    assert!(
        code.contains("def _trampoline(data_ptr, data_len, level, _context):"),
        "trampoline signature has flattened slots: {code}"
    );
    assert!(
        code.contains("bytes(data_ptr[:data_len]) if data_ptr else b\"\""),
        "bytes param converts: {code}"
    );
    assert!(
        code.contains("Level(level)"),
        "enum param converts to IntEnum: {code}"
    );
}

/// `ctypes.CFUNCTYPE` instances pin the C trampoline. Because the wrapper
/// suspends at `await` and its frame can be torn down by cancellation, the
/// trampoline is registered in the module-level `_async_pending` dict
/// under an integer token, and the completion callback pops that entry
/// before resolving the future (skipping a cancelled future).
#[test]
fn python_async_pins_callback_for_lifetime() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "fetch_data".into(),
        params: vec![Param {
            name: "id".into(),
            ty: TypeRef::I32,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::StringUtf8),
        doc: None,
        throws: false,
        r#async: true,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);
    let code = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    let pin_count = code.matches("_cb = _cb_type(_cb_impl)").count();
    assert_eq!(
        pin_count, 1,
        "expected one `_cb = _cb_type(_cb_impl)` per async fn, got {pin_count}: {code}"
    );
    // The module-level registry and its helper are emitted once.
    assert!(
        code.contains("_async_pending: Dict[int, object] = {}"),
        "missing pending-trampoline registry: {code}"
    );
    assert!(
        code.contains("def _async_register(cb) -> int:"),
        "missing _async_register helper: {code}"
    );
    // Every registration is matched by a pop on completion, and a
    // cancelled future is left untouched.
    let register_count = code.matches("_token = _async_register(_cb)").count();
    let pop_count = code.matches("_async_pending.pop(_token, None)").count();
    assert_eq!(register_count, 1, "expected one registration: {code}");
    assert_eq!(
        register_count, pop_count,
        "every registration must be popped on completion: {code}"
    );
    assert!(
        code.contains("if _fut.cancelled():"),
        "missing cancelled-future guard: {code}"
    );
}

#[test]
fn python_pyi_async_function() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "fetch_data".into(),
        params: vec![Param {
            name: "id".into(),
            ty: TypeRef::I32,
            mutable: false,
            doc: None,
        }],
        returns: Some(TypeRef::StringUtf8),
        doc: None,
        throws: false,
        r#async: true,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);
    let stubs = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        stubs.contains("async def fetch_data(id: int) -> str: ..."),
        "pyi should declare async def: {stubs}"
    );
}

#[test]
fn python_cross_module_struct() {
    let api = make_api(vec![
        Module {
            name: "types".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Name".into(),
                doc: None,
                fields: vec![StructField {
                    name: "value".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        },
        Module {
            name: "ops".into(),
            functions: vec![Function {
                name: "get_name".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Record("types.Name".into())),
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        },
    ]);

    let code = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    let stubs = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");

    assert!(
        code.contains("return _unpack_Name(_data)"),
        "cross-module return should decode via _unpack_Name: {code}"
    );
    assert!(
        !code.contains("types.Name"),
        "dot-qualified name should not appear in generated Python code: {code}"
    );
    assert!(
        stubs.contains("\"Name\""),
        "pyi should use local type name: {stubs}"
    );
    assert!(
        !stubs.contains("types.Name"),
        "dot-qualified name should not appear in pyi stubs: {stubs}"
    );
}

#[test]
fn python_nested_module_output() {
    let api = make_api(vec![Module {
        name: "parent".to_string(),
        functions: vec![Function {
            name: "outer_fn".to_string(),
            params: vec![],
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
        interfaces: vec![],
        errors: None,
        modules: vec![Module {
            name: "child".to_string(),
            functions: vec![Function {
                name: "inner_fn".to_string(),
                params: vec![],
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("# === Module: parent ==="),
        "parent module section missing: {py}"
    );
    assert!(
        py.contains("# === Module: parent_child ==="),
        "nested child module section missing: {py}"
    );
    assert!(
        py.contains("weaveffi_parent_outer_fn"),
        "parent C function missing: {py}"
    );
    assert!(
        py.contains("weaveffi_parent_child_inner_fn"),
        "nested child C function missing: {py}"
    );
    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        pyi.contains("def inner_fn"),
        "nested child function missing from pyi: {pyi}"
    );
}

#[test]
fn python_type_hint_iterator() {
    assert_eq!(
        py_type_hint(&TypeRef::Iterator(Box::new(TypeRef::I32))),
        "Iterator[int]"
    );
    assert_eq!(
        py_type_hint(&TypeRef::Iterator(Box::new(TypeRef::Record(
            "Contact".into()
        )))),
        "Iterator[\"Contact\"]"
    );
}

#[test]
fn python_iterator_return() {
    let api = make_api(vec![Module {
        name: "data".to_string(),
        functions: vec![Function {
            name: "list_items".to_string(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("class _ListItemsIterator:"),
        "should emit iterator helper class: {py}"
    );
    // Lazy contract: the wrapper hands back the helper instance; nothing
    // drains the stream into a list.
    assert!(
        py.contains("def list_items() -> Iterator[int]:"),
        "wrapper should be typed Iterator[int]: {py}"
    );
    assert!(
        py.contains("return _ListItemsIterator(_result)"),
        "wrapper should return the iterator instance: {py}"
    );
    assert!(
        !py.contains("_items = []"),
        "eager draining must be gone: {py}"
    );
    // One producer pull per step, and disposal is single-shot via
    // exhaustion, close(), or garbage collection.
    assert!(py.contains("def __next__(self):"), "missing __next__: {py}");
    assert!(
        py.contains("_next_fn = _lib.weaveffi_data_ListItemsIterator_next"),
        "missing per-step next call: {py}"
    );
    assert!(py.contains("def close(self):"), "missing close(): {py}");
    assert!(py.contains("def __del__(self):"), "missing __del__: {py}");
    assert!(
        py.contains("_destroy_fn = _lib.weaveffi_data_ListItemsIterator_destroy"),
        "missing destroy call: {py}"
    );
}

#[test]
fn python_iterator_string_elements_freed() {
    let api = make_api(vec![Module {
        name: "data".to_string(),
        functions: vec![Function {
            name: "list_names".to_string(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    // The out slot is a raw address so the pointer survives to be freed;
    // each yielded string is copied and released via `_take_string`.
    assert!(
        py.contains("_out_item = ctypes.c_void_p()"),
        "string out slot must be c_void_p: {py}"
    );
    assert!(
        py.contains("return _take_string(_out_item.value)"),
        "yielded string must be copied then freed: {py}"
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: Some("Use add_v2 instead".into()),
        since: Some("0.1.0".into()),
    }])]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("warnings.warn(\"Use add_v2 instead\", DeprecationWarning, stacklevel=2)"),
        "missing deprecation warning: {py}"
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
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn python_emits_doc_on_function() {
    let py = render_python_module(&doc_api(), true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("\"\"\"Performs a thing."), "{py}");
}

#[test]
fn python_emits_doc_on_struct() {
    let py = render_python_module(&doc_api(), true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("\"\"\"An item we track.\"\"\""), "{py}");
}

#[test]
fn python_emits_doc_on_enum_variant() {
    let py = render_python_module(&doc_api(), true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("\"\"\"Kind of item.\"\"\""), "{py}");
    assert!(py.contains("# A small one"), "{py}");
}

#[test]
fn python_emits_doc_on_field() {
    let py = render_python_module(&doc_api(), true, "weaveffi", "weaveffi.yml");
    // Field docs surface as a comment above the dataclass field.
    assert!(py.contains("# Stable id"), "{py}");
}

#[test]
fn python_emits_doc_on_param() {
    let py = render_python_module(&doc_api(), true, "weaveffi", "weaveffi.yml");
    assert!(py.contains("Parameters"), "{py}");
    assert!(py.contains("x : the input value"), "{py}");
}

#[test]
fn python_custom_prefix_threads_to_user_symbols() {
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
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);

    let py = render_python_module(&api, true, "myffi", "weaveffi.yml");

    // User symbols honor the configured ABI prefix.
    assert!(
        py.contains("_lib.myffi_math_add"),
        "user symbol should use the custom prefix: {py}"
    );
    assert!(
        !py.contains("weaveffi_math_add"),
        "user symbol must not hard-code the weaveffi_ prefix: {py}"
    );

    // Runtime ABI helpers stay literal regardless of the user prefix.
    assert!(
        py.contains("weaveffi_error_clear"),
        "runtime ABI helper must remain literal: {py}"
    );
    assert!(
        !py.contains("myffi_error_clear"),
        "runtime ABI helper must not be prefixed: {py}"
    );
}

/// A `kv` module declaring a `KvError` domain, a throwing and a
/// non-throwing free function, and a `Store` interface exercising the
/// canonical `new` constructor, a factory constructor, an instance method
/// with a string parameter and return, and a static.
fn kv_api() -> ResolvedApi {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};

    let fn_lit =
        |name: &str, params: Vec<Param>, returns: Option<TypeRef>, throws: bool| Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        };
    let str_param = |name: &str| Param {
        name: name.into(),
        ty: TypeRef::StringUtf8,
        mutable: false,
        doc: None,
    };

    make_api(vec![Module {
        name: "kv".into(),
        functions: vec![
            fn_lit(
                "lookup",
                vec![str_param("key")],
                Some(TypeRef::StringUtf8),
                true,
            ),
            fn_lit("reset", vec![], None, false),
        ],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store handle.".into()),
            constructors: vec![
                fn_lit("new", vec![str_param("path")], None, true),
                fn_lit("open_readonly", vec![str_param("path")], None, true),
            ],
            methods: vec![fn_lit(
                "get",
                vec![str_param("key")],
                Some(TypeRef::StringUtf8),
                true,
            )],
            statics: vec![fn_lit("version", vec![], Some(TypeRef::StringUtf8), false)],
        }],
        errors: Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KEY_NOT_FOUND".into(),
                    code: 1,
                    message: "key not found".into(),
                    doc: Some("Raised when the key is absent.".into()),
                    fields: vec![],
                },
                ErrorCode {
                    name: "IO_FAILURE".into(),
                    code: 2,
                    message: "io failure".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }])
}

#[test]
fn python_interface_async_and_iterator_members() {
    use weaveffi_ir::ir::InterfaceDef;
    let mut api = kv_api().api().clone();
    api.modules[0].interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: None,
        constructors: vec![Function {
            name: "new".into(),
            params: vec![],
            returns: None,
            doc: None,
            throws: true,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        methods: vec![
            Function {
                name: "fetch".into(),
                params: vec![Param {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                throws: true,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "list_keys".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: true,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        statics: vec![Function {
            name: "default_path".into(),
            params: vec![],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    }];
    let api = ResolvedApi::assume_resolved(api);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // Async method: the wrapper itself is the async def, and the launcher
    // receives self._ptr, the marshalled params, the trampoline, and the
    // NULL context.
    assert!(
        py.contains("import asyncio"),
        "missing asyncio import: {py}"
    );
    assert!(
        py.contains("async def fetch(self, key: str) -> str:"),
        "missing async def method: {py}"
    );
    assert!(
        py.contains("_fn(self._ptr, _string_to_bytes(key), _cb, None)"),
        "async launcher should receive self._ptr first: {py}"
    );
    assert!(
        !py.contains("run_in_executor"),
        "executor-based async must be gone: {py}"
    );
    // A throwing async member maps errors through the domain factory,
    // handing over the copied payload buffer.
    assert!(
        py.contains("_state[\"err\"] = _kv_error_from(_code, _msg, _payload)"),
        "throwing async member should build domain errors: {py}"
    );

    // Iterator method: the helper class is emitted at module scope,
    // qualified by the interface name, and the wrapper hands it back
    // without draining.
    assert!(
        py.contains("class _StoreListKeysIterator:"),
        "missing interface-qualified iterator helper: {py}"
    );
    assert!(
        py.contains("def list_keys(self) -> Iterator[str]:"),
        "missing iterator method: {py}"
    );
    assert!(
        py.contains("return _StoreListKeysIterator(_result)"),
        "iterator method should return the helper instance: {py}"
    );
    // Per-next errors route through the domain checker for a throwing
    // member.
    assert!(
        py.contains("_check_kv_error(_err)"),
        "iterator next should use the domain checker: {py}"
    );

    // Async static: also callback-driven; a non-throwing member falls
    // back to the generic error.
    assert!(
        py.contains("async def default_path() -> str:"),
        "missing async static wrapper: {py}"
    );
    assert!(
        py.contains("_state[\"err\"] = WeaveFFIError(_code, _msg)"),
        "non-throwing async member keeps the generic error: {py}"
    );
}

#[test]
fn python_typed_error_domain_classes() {
    let py = render_python_module(&kv_api(), true, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("class KvError(WeaveFFIError):"),
        "missing domain base class: {py}"
    );
    assert!(
        py.contains("class KeyNotFound(KvError):"),
        "missing per-code subclass: {py}"
    );
    assert!(py.contains("CODE = 1"), "missing CODE attr: {py}");
    assert!(
        py.contains("class IoFailure(KvError):"),
        "missing second per-code subclass: {py}"
    );
    assert!(
        py.contains("\"\"\"Raised when the key is absent.\"\"\""),
        "per-code class should carry its doc: {py}"
    );
    assert!(
        py.contains("\"\"\"io failure\"\"\""),
        "per-code class should fall back to its message: {py}"
    );
    assert!(
        py.contains("def __init__(self, message: str = \"key not found\") -> None:"),
        "per-code class should default its message: {py}"
    );
    assert!(
        py.contains("KvError.KeyNotFound = KeyNotFound"),
        "code classes should attach to the domain for scoped catches: {py}"
    );
    assert!(
        py.contains("1: KeyNotFound,"),
        "missing code table entry: {py}"
    );
    assert!(
        py.contains(
            "def _kv_error_from(code: int, message: str, payload: bytes = b\"\") \
             -> WeaveFFIError:"
        ),
        "missing factory: {py}"
    );
    assert!(
        py.contains("def _check_kv_error(err: _WeaveFFIErrorStruct) -> None:"),
        "missing domain checker: {py}"
    );
    assert!(
        py.contains("raise _kv_error_from(code, message, payload)"),
        "checker should raise through the factory: {py}"
    );
}

#[test]
fn python_throwing_fn_uses_domain_checker() {
    let py = render_python_module(&kv_api(), true, "weaveffi", "weaveffi.yml");
    let lookup = py
        .split("def lookup(")
        .nth(1)
        .expect("lookup wrapper present");
    let lookup_body = lookup.split("\n\n").next().unwrap();
    assert!(
        lookup_body.contains("_check_kv_error(_err)"),
        "throwing fn should route through the domain checker: {py}"
    );
}

#[test]
fn python_non_throwing_fn_uses_generic_checker() {
    let py = render_python_module(&kv_api(), true, "weaveffi", "weaveffi.yml");
    let reset = py
        .split("def reset(")
        .nth(1)
        .expect("reset wrapper present");
    let reset_body = reset.split("\n\n").next().unwrap();
    assert!(
        reset_body.contains("def reset() -> None:")
            || reset.starts_with(") -> None:")
            || py.contains("def reset() -> None:"),
        "non-throwing fn keeps a plain signature: {py}"
    );
    assert!(
        reset_body.contains("_check_error(_err)"),
        "non-throwing fn should use the generic checker: {py}"
    );
    assert!(
        !reset_body.contains("_check_kv_error"),
        "non-throwing fn must not use the domain checker: {py}"
    );
}

#[test]
fn python_interface_class_generated() {
    let py = render_python_module(&kv_api(), true, "weaveffi", "weaveffi.yml");

    assert!(py.contains("class Store:"), "missing wrapper class: {py}");
    assert!(
        py.contains("\"\"\"A key-value store handle.\"\"\""),
        "missing interface docstring: {py}"
    );
    assert!(
        py.contains("def _from_ptr(cls, ptr) -> \"Store\":"),
        "missing _from_ptr wrapper hook: {py}"
    );

    // `new` becomes `__init__`, calling the constructor symbol and
    // stashing the owned pointer.
    assert!(
        py.contains("def __init__(self, path: str) -> None:"),
        "missing __init__ from ctor `new`: {py}"
    );
    assert!(
        py.contains("_lib.weaveffi_kv_Store_new"),
        "missing ctor symbol: {py}"
    );
    assert!(
        py.contains("self._ptr = _result"),
        "__init__ should own the returned pointer: {py}"
    );

    // The second constructor is a classmethod factory.
    assert!(
        py.contains("@classmethod\n    def open_readonly(cls, path: str) -> \"Store\":"),
        "missing classmethod factory: {py}"
    );
    assert!(
        py.contains("return cls._from_ptr(_result)"),
        "factory should wrap via _from_ptr: {py}"
    );

    // Instance method: string param and return, `self._ptr` leading arg.
    assert!(
        py.contains("def get(self, key: str) -> str:"),
        "missing method signature: {py}"
    );
    assert!(
        py.contains("_fn(self._ptr, _string_to_bytes(key), ctypes.byref(_err))"),
        "method should pass self._ptr as the leading C argument: {py}"
    );
    let get_body = py.split("def get(").nth(1).unwrap();
    let get_body = get_body.split("\n\n").next().unwrap();
    assert!(
        get_body.contains("_check_kv_error(_err)"),
        "throwing method should use the domain checker: {py}"
    );

    // Static member.
    assert!(
        py.contains("@staticmethod\n    def version() -> str:"),
        "missing staticmethod: {py}"
    );
    assert!(
        py.contains("_lib.weaveffi_kv_Store_version"),
        "missing static symbol: {py}"
    );

    // Destroy wiring in __del__.
    assert!(
        py.contains("_lib.weaveffi_kv_Store_destroy(self._ptr)"),
        "missing destroy call in __del__: {py}"
    );
}

#[test]
fn python_interface_param_and_return_marshalling() {
    use weaveffi_ir::ir::InterfaceDef;
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![
            Function {
                name: "clone_store".into(),
                params: vec![Param {
                    name: "store".into(),
                    ty: TypeRef::Interface("Store".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Interface("Store".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "find_store".into(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Store".into(),
                )))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
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
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // An interface parameter is borrowed: the wrapper passes its pointer.
    assert!(
        py.contains("def clone_store(store: \"Store\") -> \"Store\":"),
        "missing interface hints: {py}"
    );
    assert!(
        py.contains("_fn(store._ptr, ctypes.byref(_err))"),
        "interface param should pass ._ptr: {py}"
    );
    // A returned interface wraps the owned pointer via _from_ptr.
    assert!(
        py.contains("return Store._from_ptr(_result)"),
        "interface return should wrap via _from_ptr: {py}"
    );
    // An optional interface return maps null to None.
    let find = py.split("def find_store(").nth(1).unwrap();
    assert!(
        find.contains("return None") && find.contains("Store._from_ptr(_result)"),
        "optional interface return should null-check: {py}"
    );
}

#[test]
fn python_naming_default_stripped_snake_case() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "createContact".into(),
            params: vec![Param {
                name: "firstName".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let config = PythonConfig::default();
    assert!(config.strip_module_prefix, "stripping must be the default");
    let py = render_python_module(&api, config.strip_module_prefix, "weaveffi", "weaveffi.yml");
    assert!(
        py.contains("def create_contact(first_name: str) -> int:"),
        "default naming should be bare snake_case incl. params: {py}"
    );
    assert!(
        py.contains("_fn(_string_to_bytes(first_name), ctypes.byref(_err))"),
        "body references should use the snake_case param name: {py}"
    );
    assert!(
        !py.contains("def contacts_create_contact("),
        "default should not module-prefix: {py}"
    );
}

#[test]
fn python_throws_docstring_has_raises_section() {
    let py = render_python_module(&kv_api(), true, "weaveffi", "weaveffi.yml");
    let lookup = py
        .split("def lookup(")
        .nth(1)
        .expect("lookup wrapper present");
    assert!(
        lookup.contains("Raises\n    ------\n    KvError\n"),
        "throwing fn should document Raises: {py}"
    );
    let reset = py
        .split("def reset(")
        .nth(1)
        .expect("reset wrapper present");
    let reset_body = reset.split("\n\n").next().unwrap();
    assert!(
        !reset_body.contains("Raises"),
        "non-throwing fn must not document domain raises: {py}"
    );
}

#[test]
fn python_pyi_errors_and_interfaces() {
    let pyi = render_pyi_module(
        &BindingModel::build(&kv_api(), "weaveffi"),
        true,
        "weaveffi.yml",
    );
    assert!(
        pyi.contains("class WeaveFFIError(Exception):"),
        "stub should declare the generic error: {pyi}"
    );
    assert!(
        pyi.contains("class KvError(WeaveFFIError):"),
        "stub should declare the domain base: {pyi}"
    );
    assert!(
        pyi.contains("class KeyNotFound(KvError):"),
        "stub should declare per-code classes: {pyi}"
    );
    assert!(
        pyi.contains("    KeyNotFound: Type[\"KeyNotFound\"]"),
        "stub should declare the scoped alias on the domain: {pyi}"
    );
    assert!(
        pyi.contains("class Store:"),
        "stub should declare the interface class: {pyi}"
    );
    assert!(
        pyi.contains("def __init__(self, path: str) -> None: ..."),
        "stub should declare __init__ for ctor `new`: {pyi}"
    );
    assert!(
        pyi.contains("def open_readonly(cls, path: str) -> \"Store\": ..."),
        "stub should declare factory classmethods: {pyi}"
    );
    assert!(
        pyi.contains("def get(self, key: str) -> str: ..."),
        "stub should declare methods: {pyi}"
    );
    assert!(
        pyi.contains("def version() -> str: ..."),
        "stub should declare statics: {pyi}"
    );
}

#[test]
fn preamble_includes_buffer_runtime() {
    let api = make_api(vec![simple_module(vec![Function {
        name: "noop".into(),
        params: vec![],
        returns: None,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }])]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
    // The wire-format codec runtime ships in every generated module.
    assert!(py.contains("import struct"), "missing struct import: {py}");
    assert!(
        py.contains("class _BufferWriter:"),
        "missing buffer writer: {py}"
    );
    assert!(
        py.contains("class _BufferReader:"),
        "missing buffer reader: {py}"
    );
    assert!(
        py.contains("def _decode_buffer(data: bytes, read_fn):"),
        "missing decode helper: {py}"
    );
    assert!(
        py.contains("def _take_buffer(ptr, length) -> bytes:"),
        "missing take helper: {py}"
    );
    // Little-endian packed encodings.
    assert!(
        py.contains("struct.pack(\"<i\", v)"),
        "missing little-endian i32 pack: {py}"
    );
    // Malformed-buffer rejection paths.
    assert!(
        py.contains("malformed value buffer: truncated"),
        "missing truncation rejection: {py}"
    );
    assert!(
        py.contains("malformed value buffer: trailing bytes"),
        "missing trailing-bytes rejection: {py}"
    );
    assert!(
        py.contains("length prefix exceeds remaining bytes"),
        "missing length-prefix rejection: {py}"
    );
    // The error struct carries the structured payload slots.
    assert!(
        py.contains("(\"payload_ptr\", ctypes.c_void_p),"),
        "missing payload_ptr field: {py}"
    );
    assert!(
        py.contains("(\"payload_len\", ctypes.c_size_t),"),
        "missing payload_len field: {py}"
    );
}

#[test]
fn rich_enum_sum_type() {
    let api = make_api(vec![Module {
        name: "shapes".into(),
        functions: vec![Function {
            name: "area".into(),
            params: vec![Param {
                name: "shape".into(),
                ty: TypeRef::RichEnum("Shape".into()),
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::F64),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Shape".into(),
            doc: Some("A closed figure.".into()),
            variants: vec![
                EnumVariant {
                    name: "Circle".into(),
                    value: 0,
                    doc: None,
                    fields: vec![StructField {
                        name: "radius".into(),
                        ty: TypeRef::F64,
                        doc: None,
                    }],
                },
                EnumVariant {
                    name: "Rect".into(),
                    value: 1,
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "width".into(),
                            ty: TypeRef::F64,
                            doc: None,
                        },
                        StructField {
                            name: "height".into(),
                            ty: TypeRef::F64,
                            doc: None,
                        },
                    ],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // Base class with the nested Tag discriminant and a tag property.
    assert!(py.contains("class Shape:"), "missing base class: {py}");
    assert!(
        py.contains("class Tag(IntEnum):"),
        "missing nested Tag enum: {py}"
    );
    assert!(py.contains("Circle = 0"), "missing tag value: {py}");
    assert!(
        py.contains("def tag(self) -> \"Shape.Tag\":"),
        "missing tag property: {py}"
    );

    // One dataclass subclass per variant, with scoped aliases.
    assert!(
        py.contains("@dataclass\nclass ShapeCircle(Shape):"),
        "missing Circle variant dataclass: {py}"
    );
    assert!(
        py.contains("TAG = Shape.Tag.Circle"),
        "missing Circle TAG: {py}"
    );
    assert!(py.contains("radius: float"), "missing Circle field: {py}");
    assert!(
        py.contains("@dataclass\nclass ShapeRect(Shape):"),
        "missing Rect variant dataclass: {py}"
    );
    assert!(
        py.contains("Shape.Circle = ShapeCircle"),
        "missing scoped alias: {py}"
    );

    // No FFI symbols exist for rich enums.
    assert!(
        !py.contains("Shape_new_") && !py.contains("Shape_tag") && !py.contains("Shape_destroy"),
        "rich enums must not call C symbols: {py}"
    );

    // The codec dispatches on isinstance and the i32 wire tag.
    assert!(
        py.contains("def _write_Shape(_w: _BufferWriter, value: \"Shape\") -> None:"),
        "missing writer: {py}"
    );
    assert!(
        py.contains("if isinstance(value, ShapeCircle):"),
        "missing isinstance dispatch: {py}"
    );
    assert!(
        py.contains("_w.write_f64(value.radius)"),
        "missing variant field write: {py}"
    );
    assert!(
        py.contains("def _read_Shape(_r: _BufferReader) -> \"Shape\":"),
        "missing reader: {py}"
    );
    assert!(
        py.contains("_tag = _r.read_i32()"),
        "missing tag read: {py}"
    );
    assert!(
        py.contains("radius=_r.read_f64(),"),
        "missing variant field read: {py}"
    );
    assert!(
        py.contains("unknown Shape tag"),
        "missing unknown-tag rejection: {py}"
    );

    // The rich enum parameter packs like any buffered value.
    assert!(
        py.contains("_shape_buf = _pack_Shape(shape)"),
        "missing param pack: {py}"
    );
    assert!(
        py.contains("_fn(_shape_buf, len(_shape_buf), ctypes.byref(_err))"),
        "missing buffered call args: {py}"
    );
}

#[test]
fn error_payload_decoding() {
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain};
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![Function {
            name: "get".into(),
            params: vec![Param {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
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
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KEY_NOT_FOUND".into(),
                    code: 1,
                    message: "key not found".into(),
                    doc: None,
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
                    name: "IO_FAILURE".into(),
                    code: 2,
                    message: "io failure".into(),
                    doc: None,
                    fields: vec![],
                },
            ],
        }),
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // The payload decoder reads the code's fields in declaration order
    // and attaches them as exception attributes.
    assert!(
        py.contains(
            "def _kv_error_payload_key_not_found(_exc: WeaveFFIError, \
             _r: _BufferReader) -> None:"
        ),
        "missing payload decoder: {py}"
    );
    assert!(
        py.contains("_exc.key = _r.read_string()"),
        "missing key attribute decode: {py}"
    );
    assert!(
        py.contains("_exc.attempts = _r.read_i32()"),
        "missing attempts attribute decode: {py}"
    );
    // Only codes with fields enter the payload table.
    assert!(
        py.contains("_KV_ERROR_PAYLOADS: Dict[int, Callable] = {"),
        "missing payload table: {py}"
    );
    assert!(
        py.contains("1: _kv_error_payload_key_not_found,"),
        "missing payload table entry: {py}"
    );
    assert!(
        !py.contains("_kv_error_payload_io_failure"),
        "field-less code must not get a decoder: {py}"
    );
    // The factory decodes the payload; the checker copies it before
    // weaveffi_error_clear frees it.
    assert!(
        py.contains("_decoder = _KV_ERROR_PAYLOADS.get(code)"),
        "factory should look up the decoder: {py}"
    );
    assert!(
        py.contains(
            "payload = ctypes.string_at(err.payload_ptr, err.payload_len) \
             if err.payload_ptr else b\"\""
        ),
        "checker should copy the payload before clearing: {py}"
    );
    assert!(
        py.contains("raise _kv_error_from(code, message, payload)"),
        "checker should raise through the factory with the payload: {py}"
    );
    // The stub declares the payload attributes on the code class.
    let pyi = render_pyi_module(&BindingModel::build(&api, "weaveffi"), true, "weaveffi.yml");
    assert!(
        pyi.contains("class KeyNotFound(KvError):"),
        "stub should declare the code class: {pyi}"
    );
    assert!(
        pyi.contains("    key: str\n") && pyi.contains("    attempts: int\n"),
        "stub should declare payload attributes: {pyi}"
    );
}

#[test]
fn listener_buffered_record_param_decodes() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "events".into(),
        functions: vec![],
        structs: vec![StructDef {
            name: "Event".into(),
            doc: None,
            fields: vec![StructField {
                name: "kind".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![CallbackDef {
            name: "OnEvent".into(),
            params: vec![Param {
                name: "event".into(),
                ty: TypeRef::Record("Event".into()),
                mutable: false,
                doc: None,
            }],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "event_feed".into(),
            event_callback: "OnEvent".into(),
            doc: None,
        }],
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    // The borrowed (ptr, len) pair is copied and decoded before the
    // user callable runs; nothing is freed on the consumer side.
    assert!(
        py.contains(
            "_unpack_Event(ctypes.string_at(event_ptr, event_len) \
             if event_ptr else b\"\")"
        ),
        "listener trampoline should decode the borrowed record: {py}"
    );
    assert!(
        py.contains("ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t"),
        "trampoline CFUNCTYPE should carry the (ptr, len) slots: {py}"
    );
}

#[test]
fn async_function_returns_buffered_record() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "fetch_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I64,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("async def fetch_contact(id: int) -> \"Contact\":"),
        "missing async wrapper: {py}"
    );
    // The owned result buffer is copied and freed by `_take_buffer`, then
    // decoded inside the completion trampoline.
    assert!(
        py.contains(
            "_state[\"val\"] = _unpack_Contact(\
             _take_buffer(ctypes.cast(result_ptr, ctypes.c_void_p).value, result_len))"
        ),
        "trampoline should decode the owned result buffer: {py}"
    );
    // Decode failures surface through the future rather than escaping
    // the C callback.
    assert!(
        py.contains("except Exception as _exc:"),
        "decode errors should be trapped: {py}"
    );
    assert!(
        py.contains("_state[\"err\"] = _exc"),
        "trapped decode errors should resolve the future: {py}"
    );
}

#[test]
fn iterator_buffered_record_elements() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "iter_contacts".into(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
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
        interfaces: vec![],
        errors: None,
        modules: vec![],
    }]);
    let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

    assert!(
        py.contains("def iter_contacts() -> Iterator[\"Contact\"]:"),
        "missing iterator wrapper: {py}"
    );
    // The `_next` signature carries the encoded item pointer plus its
    // trailing out_len.
    assert!(
        py.contains(
            "_next_fn.argtypes = [ctypes.c_void_p, \
             ctypes.POINTER(ctypes.c_void_p), \
             ctypes.POINTER(ctypes.c_size_t), \
             ctypes.POINTER(_WeaveFFIErrorStruct)]"
        ),
        "next should take out_item and out_len: {py}"
    );
    // Each element is copied, freed with weaveffi_free_bytes (via
    // _take_buffer), then decoded.
    assert!(
        py.contains("return _unpack_Contact(_take_buffer(_out_item.value, _out_len.value))"),
        "element should be taken then decoded: {py}"
    );
}

/// A kitchen-sink API touching every buffered surface at once (record and
/// rich enum params and returns, nested optional/list/map fields, error
/// payloads, a buffered listener, an async buffered method, and a record
/// iterator) generates a complete package without panicking.
#[test]
fn kitchen_sink_api_generates() {
    use weaveffi_ir::ir::{CallbackDef, ErrorCode, ErrorDomain, InterfaceDef, ListenerDef};
    let api = make_api(vec![Module {
        name: "kitchen".into(),
        functions: vec![
            Function {
                name: "save".into(),
                params: vec![
                    Param {
                        name: "item".into(),
                        ty: TypeRef::Record("Item".into()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "shape".into(),
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "tags".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "scores".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "note".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Record("Item".into())),
                doc: None,
                throws: true,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "fetch".into(),
                params: vec![],
                returns: Some(TypeRef::RichEnum("Shape".into())),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "stream".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record("Item".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "quantity".into(),
                    ty: TypeRef::U32,
                    doc: None,
                },
                StructField {
                    name: "data".into(),
                    ty: TypeRef::Bytes,
                    doc: None,
                },
                StructField {
                    name: "nested".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::F64),
                    ))))),
                    doc: None,
                },
            ],
        }],
        enums: vec![EnumDef {
            name: "Shape".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Dot".into(),
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
            ],
        }],
        callbacks: vec![CallbackDef {
            name: "OnItem".into(),
            params: vec![Param {
                name: "item".into(),
                ty: TypeRef::Record("Item".into()),
                mutable: false,
                doc: None,
            }],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "item_feed".into(),
            event_callback: "OnItem".into(),
            doc: None,
        }],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![Function {
                name: "new".into(),
                params: vec![],
                returns: None,
                doc: None,
                throws: true,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            methods: vec![Function {
                name: "put".into(),
                params: vec![Param {
                    name: "item".into(),
                    ty: TypeRef::Record("Item".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record("Item".into())))),
                doc: None,
                throws: true,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            statics: vec![],
        }],
        errors: Some(ErrorDomain {
            name: "KitchenError".into(),
            codes: vec![ErrorCode {
                name: "BAD_ITEM".into(),
                code: 1,
                message: "bad item".into(),
                doc: None,
                fields: vec![StructField {
                    name: "reason".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
        }),
        modules: vec![],
    }]);
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    PythonGenerator
        .generate(&api, out, &PythonConfig::default())
        .unwrap();
    let py = std::fs::read_to_string(out.join("python/weaveffi/weaveffi.py")).unwrap();
    assert!(py.contains("def _pack_Item("), "missing Item codec: {py}");
    assert!(py.contains("def _read_Shape("), "missing Shape codec: {py}");
    assert!(
        py.contains("_kitchen_error_payload_bad_item"),
        "missing payload decoder: {py}"
    );
    assert!(
        std::fs::metadata(out.join("python/weaveffi/weaveffi.pyi")).is_ok(),
        "missing stub file"
    );
}
