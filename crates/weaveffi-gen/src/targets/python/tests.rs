//! Unit tests: render a small API that exercises every ABI revision 3 shape
//! (prefixed runtime, pointer + length strings, reference-counted objects,
//! nullable objects, objects inside buffers, iterators, async and
//! cancellable functions, and callback interfaces) and assert the generated
//! Python carries the pieces each contract clause requires.

use crate::backend::{LanguageBackend, OutputFile};
use crate::package::PackageContext;
use crate::platform::{BinarySet, Platform};
use camino::Utf8Path;
use weaveffi_model::ir::{
    Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
    TypeRef, CURRENT_SCHEMA_VERSION,
};
use weaveffi_model::model::BindingModel;
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;
use weaveffi_model::validate::validate_api;

use crate::targets::python::{PythonConfig, PythonGenerator};

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

fn module(name: &str) -> Module {
    Module {
        name: name.into(),
        doc: None,
        functions: vec![],
        interfaces: vec![],
        callback_interfaces: vec![],
        structs: vec![],
        enums: vec![],
        errors: None,
        modules: vec![],
    }
}

fn resolve(modules: Vec<Module>) -> ResolvedApi {
    let api = Api {
        version: CURRENT_SCHEMA_VERSION.into(),
        modules,
    };
    validate_api(api, None)
        .unwrap_or_else(|d| panic!("fixture must validate: {d:?}"))
        .with_identity(Identity::named("kv-lib"))
}

/// An interface with a constructor and methods; a function taking and
/// returning `Interface?`; a record with object fields; an iterator over
/// objects; a callback interface; an async function and a cancellable one.
fn kv_api() -> ResolvedApi {
    let kv = Module {
        structs: vec![StructDef {
            name: "Entry".into(),
            doc: None,
            deprecated: None,
            fields: vec![
                field("key", TypeRef::StringUtf8),
                field("store", named("Store")),
                field("mirrors", TypeRef::List(Box::new(named("Store")))),
                field("weights", TypeRef::List(Box::new(TypeRef::I32))),
            ],
        }],
        interfaces: vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store.".into()),
            deprecated: None,
            constructors: vec![func("new", vec![param("path", TypeRef::StringUtf8)], None)],
            methods: vec![
                func(
                    "get",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                ),
                func(
                    "has",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::Bool),
                ),
            ],
            statics: vec![],
        }],
        callback_interfaces: vec![CallbackInterfaceDef {
            name: "Watcher".into(),
            doc: Some("Observes store changes.".into()),
            deprecated: None,
            methods: vec![
                func(
                    "on_change",
                    vec![
                        param("text", TypeRef::StringUtf8),
                        param("entry", named("Entry")),
                        param("store", named("Store")),
                    ],
                    None,
                ),
                func(
                    "should_stop",
                    vec![param("count", TypeRef::I32)],
                    Some(TypeRef::Bool),
                ),
            ],
        }],
        functions: vec![
            func(
                "pick",
                vec![param("store", TypeRef::Optional(Box::new(named("Store"))))],
                Some(TypeRef::Optional(Box::new(named("Store")))),
            ),
            func("watch", vec![param("watcher", named("Watcher"))], None),
            func(
                "all_stores",
                vec![],
                Some(TypeRef::Iterator(Box::new(named("Store")))),
            ),
            Function {
                r#async: true,
                ..func(
                    "fetch",
                    vec![param("id", TypeRef::I64)],
                    Some(named("Store")),
                )
            },
            Function {
                r#async: true,
                cancellable: true,
                ..func(
                    "sleep",
                    vec![param("ms", TypeRef::I64)],
                    Some(TypeRef::StringUtf8),
                )
            },
        ],
        ..module("kv")
    };
    resolve(vec![kv])
}

fn render_files(api: &ResolvedApi, config: &PythonConfig) -> Vec<OutputFile> {
    let model = BindingModel::build(api);
    PythonGenerator.files(api, &model, Utf8Path::new("out"), config)
}

/// Render the fixture and return `(module source, stub)`.
fn render(api: &ResolvedApi) -> (String, String) {
    let files = render_files(api, &PythonConfig::default());
    let find = |suffix: &str| {
        files
            .iter()
            .find(|f| f.path.as_str().ends_with(suffix))
            .unwrap_or_else(|| panic!("missing {suffix}"))
            .contents
            .clone()
    };
    (find("kv_lib.py"), find("kv_lib.pyi"))
}

fn assert_has(hay: &str, needle: &str) {
    assert!(hay.contains(needle), "missing `{needle}` in:\n{hay}");
}

#[test]
fn layout_follows_the_identity() {
    let files = render_files(&kv_api(), &PythonConfig::default());
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/python/kv_lib/__init__.py",
            "out/python/kv_lib/kv_lib.py",
            "out/python/kv_lib/kv_lib.pyi",
            "out/python/kv_lib/py.typed",
            "out/python/pyproject.toml",
            "out/python/README.md",
        ]
    );
    assert_has(&files[0].contents, "from .kv_lib import *");
    let pyproject = &files[4].contents;
    assert_has(pyproject, "name = \"kv-lib\"");
    assert_has(pyproject, "requires-python = \">=3.9\"");
    assert_has(pyproject, "packages = [\"kv_lib\"]");

    let config = PythonConfig {
        package_name: Some("kv-python".into()),
        import_name: Some("kvpy".into()),
        ..PythonConfig::default()
    };
    let files = render_files(&kv_api(), &config);
    assert!(files.iter().any(|f| f.path == "out/python/kvpy/kvpy.py"));
    let pyproject = files.iter().find(|f| f.path.ends_with("pyproject.toml"));
    assert_has(&pyproject.unwrap().contents, "name = \"kv-python\"");
}

#[test]
fn runtime_is_prefixed_and_checks_the_contract() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "_ABI_VERSION = 3\n");
    assert_has(&py, "    (\"kv\", \"kv_lib_kv_checksum\", 0x");
    assert_has(&py, "_bind(\"kv_lib_abi_version\", ctypes.c_uint32)");
    assert_has(&py, "os.environ.get(\"KV_LIB_LIBRARY\")");
    assert_has(&py, "name = \"libkv_lib.dylib\"");
    assert_has(
        &py,
        "_bind(\"kv_lib_free_bytes\", None, ctypes.c_void_p, ctypes.c_size_t)",
    );
    assert_has(&py, "CANCELLED_ERROR_CODE = -5");
    assert_has(&py, "class Error(Exception):");
    assert_has(&pyi, "class Error(Exception):");
    assert!(!py.contains("free_string"));
    // Nothing but the generated-file header names WeaveFFI.
    for text in [&py, &pyi] {
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            assert!(
                !line.to_ascii_lowercase().contains("weaveffi"),
                "branded line: {line}"
            );
        }
    }
}

#[test]
fn prototypes_are_bound_once_with_c_bool_and_ptr_len_strings() {
    let (py, _) = render(&kv_api());
    assert_has(
        &py,
        "_c_kv_Store_has = _bind(\"kv_lib_kv_Store_has\", ctypes.c_bool, ctypes.c_void_p, \
         ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_ErrorStruct))",
    );
    assert_eq!(py.matches("_c_kv_Store_get = _bind(").count(), 1);
    assert!(!py.contains(".argtypes = ["), "no per-call binding");
    // A string crosses as UTF-8 bytes plus a length and comes back owned.
    assert_has(&py, "_key_b = key.encode(\"utf-8\")");
    assert_has(&py, "_key_b, len(_key_b)");
    assert_has(&py, "return _take_str(_ret, _out_len.value)");
}

#[test]
fn objects_are_lent_for_calls_and_adopted_from_returns() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "class Store(_Object):");
    assert_has(&py, "    _destroy = staticmethod(_c_kv_Store_destroy)\n");
    assert_has(&py, "    _clone = staticmethod(_c_kv_Store_clone)\n");
    assert_has(&py, "def __init__(self, path: str) -> None:");
    assert_has(&py, "self._init_handle(_required(_ret))");
    // `self` is lent for the call and released after it, even on error.
    assert_has(
        &py,
        "_self_p = self._acquire()\n        try:\n            _ret = _c_kv_Store_get(_self_p,",
    );
    assert_has(&py, "        finally:\n            self._release()\n");
    // Nullable objects map None to NULL both ways.
    assert_has(&py, "_store_p = _lend_opt(store, Store)");
    assert_has(&py, "_release_opt(store)");
    assert_has(&py, "return (Store._adopt(_ret) if _ret else None)");
    // Objects inside buffers are cloned when the buffer is finished and
    // adopted on read; numeric lists take the packed fast path.
    assert_has(&py, "_w.write_object(value.store, Store)");
    assert_has(&py, "store=Store._adopt(_r.read_object()),");
    assert_has(&py, "_w.write_numbers(\"i\", value.weights)");
    assert_has(&py, "weights=_r.read_numbers(\"i\", 4),");
    assert_has(&pyi, "    def close(self) -> None: ...\n");
    assert_has(&pyi, "    def __enter__(self) -> \"Store\": ...\n");
}

#[test]
fn iterators_pull_lazily_through_prebound_functions() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "class _kv_AllStoresIterator(_Iterator):");
    assert_has(
        &py,
        "    _destroy = staticmethod(_c_kv_AllStoresIterator_destroy)",
    );
    assert_has(
        &py,
        "_more = _c_kv_AllStoresIterator_next(_p, ctypes.byref(_item), ctypes.byref(_err))",
    );
    assert_has(&py, "return Store._adopt(_required(_item.value))");
    assert_has(&py, "return _kv_AllStoresIterator._adopt(_ret)");
    assert_has(&pyi, "def all_stores() -> Iterator[\"Store\"]: ...");
}

#[test]
fn async_calls_complete_through_static_trampolines() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "import asyncio\n");
    assert_has(
        &py,
        "_c_kv_fetch_callback = ctypes.CFUNCTYPE(None, ctypes.c_void_p, \
         ctypes.POINTER(_ErrorStruct), ctypes.c_void_p)",
    );
    assert_has(&py, "def _kv_fetch_complete(context, err, result) -> None:");
    assert_has(
        &py,
        "_async_settle(context, None, Store._adopt(_required(result)))",
    );
    assert_has(
        &py,
        "_kv_fetch_completion = _c_kv_fetch_callback(_kv_fetch_complete)",
    );
    assert_has(&py, "_c_kv_fetch(id, _kv_fetch_completion, _call)");
    assert_has(&py, "return await _future");
    // A cancellable call owns a token for its duration.
    assert_has(&py, "_token = _cancel_token_create()");
    assert_has(&py, "_c_kv_sleep(ms, _token, _kv_sleep_completion, _call)");
    assert_has(&py, "return await _async_wait_cancellable(_future, _token)");
    assert_has(
        &py,
        "_async_settle(context, None, _take_str(result_ptr, result_len))",
    );
    assert_has(&pyi, "async def sleep(ms: int) -> str: ...");
}

#[test]
fn callback_interfaces_render_abc_vtable_and_trampolines() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "import abc\n");
    assert_has(&py, "class Watcher(abc.ABC):");
    assert_has(
        &py,
        "_Watcher_on_change_t = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, \
         ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, \
         ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(
        &py,
        "_Watcher_should_stop_t = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, \
         ctypes.c_int32, ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(&py, "class _WatcherVtable(ctypes.Structure):");
    assert_has(&py, "The C vtable `kv_lib_kv_Watcher_vtable`.");
    assert_has(
        &py,
        "_callback_get(ctx).on_change(_peek_bytes(text_ptr, text_len).decode(\"utf-8\"), \
         _decode(_peek_bytes(entry_ptr, entry_len), _read_Entry), \
         Store._adopt(_required(store)))",
    );
    assert_has(&py, "return bool(_callback_get(ctx).should_stop(count))");
    assert_has(&py, "_callback_fail(out_err, exc)\n        return False");
    assert_has(&py, "_Watcher_free_t(_callback_free),");
    assert_eq!(py.matches("_Watcher_vtable = _WatcherVtable(").count(), 1);
    assert_has(&py, "_watcher_ctx = _callback_register(watcher, Watcher)");
    assert_has(
        &py,
        "_c_kv_watch(_watcher_ctx, ctypes.addressof(_Watcher_vtable), ctypes.byref(_err))",
    );
    assert_has(&pyi, "class Watcher(ABC):");
}

#[test]
fn feature_runtime_is_emitted_only_when_used() {
    let plain = Module {
        functions: vec![func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )],
        ..module("math")
    };
    let (py, _) = render(&resolve(vec![plain]));
    assert!(!py.contains("import abc"));
    assert!(!py.contains("import asyncio"));
    assert!(!py.contains("_callbacks"));
    assert!(!py.contains("_cancel_token"));
    assert_has(&py, "def add(a: int, b: int) -> int:");
}

#[test]
fn root_error_avoids_user_type_names() {
    let m = Module {
        structs: vec![StructDef {
            name: "Error".into(),
            doc: None,
            deprecated: None,
            fields: vec![field("code", TypeRef::I32)],
        }],
        ..module("kv")
    };
    let (py, pyi) = render(&resolve(vec![m]));
    assert_has(&py, "class KvLibError(Exception):");
    assert_has(&pyi, "class KvLibError(Exception):");
}

#[test]
fn generated_source_is_deterministic() {
    let api = kv_api();
    assert_eq!(render(&api), render(&api));
}

#[test]
fn package_bundles_the_identity_library_per_wheel_platform() {
    let api = kv_api();
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("kv_lib");
    for p in Platform::ALL {
        binaries.insert(p, format!("/tmp/{}/lib", p.id()));
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("kv.yml"),
    };
    let files = PythonGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &PythonConfig::default(),
        )
        .expect("python packages");
    let trees: std::collections::BTreeSet<&str> = files
        .iter()
        .filter_map(|f| f.path.as_str().strip_prefix("out/python/"))
        .filter_map(|rest| rest.split('/').next())
        .collect();
    let expected: std::collections::BTreeSet<&str> =
        Platform::DESKTOP.iter().map(|p| p.id()).collect();
    assert_eq!(trees, expected);
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("macos-arm64/kv_lib/libkv_lib.dylib")
        || f.path.as_str().ends_with("/kv_lib/libkv_lib.dylib")));
    for p in [
        Platform::AndroidArm64,
        Platform::AndroidX64,
        Platform::Wasm32,
    ] {
        assert!(!files.iter().any(|f| f.path.as_str().contains(p.id())));
    }
}
