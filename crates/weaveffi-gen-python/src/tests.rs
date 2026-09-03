//! Unit tests: render a small API that exercises every ABI revision 2 shape
//! (reference-counted objects, nullable objects, objects inside buffers,
//! iterators of objects, and callback interfaces) and assert the generated
//! Python carries the pieces each contract clause requires.

use camino::Utf8Path;
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::PackageContext;
use weaveffi_core::platform::{BinarySet, Platform};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::validate::validate_api;
use weaveffi_ir::ir::{
    Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
    TypeRef, CURRENT_SCHEMA_VERSION,
};

use crate::{PythonConfig, PythonGenerator};

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

/// The fixture the brief asks for: an interface with a constructor and a
/// method; a function taking and returning `Interface?`; a record with an
/// `Interface` field and a `[Interface]` field; an iterator over interface
/// elements; a callback interface whose methods take a string, an `i32`, a
/// record, and an object, one returning `bool` and one returning void; and a
/// function taking that callback interface. Routed through validation so the
/// fixture is known to be a legal IDL.
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
                field("backup", TypeRef::Optional(Box::new(named("Store")))),
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
                    "sibling",
                    vec![],
                    Some(TypeRef::Optional(Box::new(named("Store")))),
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
                        param("weight", TypeRef::I32),
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
        ],
        ..module("kv")
    };
    let api = Api {
        version: CURRENT_SCHEMA_VERSION.into(),
        modules: vec![kv],
    };
    validate_api(api, None).unwrap_or_else(|d| panic!("fixture must validate: {d:?}"))
}

/// Render the fixture and return `(weaveffi.py, weaveffi.pyi)`.
fn render(api: &ResolvedApi) -> (String, String) {
    let config = PythonConfig::default();
    let model = BindingModel::build(api, config.prefix());
    let files = PythonGenerator.files(api, &model, Utf8Path::new("out"), &config);
    let find = |suffix: &str| {
        files
            .iter()
            .find(|f| f.path.as_str().ends_with(suffix))
            .unwrap_or_else(|| panic!("missing {suffix}"))
            .contents
            .clone()
    };
    (find("weaveffi.py"), find("weaveffi.pyi"))
}

fn assert_has(hay: &str, needle: &str) {
    assert!(hay.contains(needle), "missing `{needle}` in:\n{hay}");
}

#[test]
fn runtime_binds_error_set_and_drops_legacy_symbols() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "_ABI_VERSION = 2\n");
    assert_has(&py, "_lib.weaveffi_error_set.argtypes = [");
    assert_has(&py, "FOREIGN_ERROR_CODE = -4");
    for legacy in ["arena", "handle_t", "listener", "register_"] {
        assert!(!py.contains(legacy), "legacy `{legacy}` survives in:\n{py}");
        assert!(
            !pyi.contains(legacy),
            "legacy `{legacy}` survives in:\n{pyi}"
        );
    }
    assert_has(&pyi, "    FOREIGN_ERROR_CODE: int\n");
}

#[test]
fn objects_are_reference_counted_wrappers() {
    let (py, pyi) = render(&kv_api());
    // The clone/destroy pair is bound once at module scope.
    assert_has(
        &py,
        "_lib.weaveffi_kv_Store_clone.argtypes = [ctypes.c_void_p]",
    );
    assert_has(
        &py,
        "_lib.weaveffi_kv_Store_clone.restype = ctypes.c_void_p",
    );
    assert_has(
        &py,
        "_lib.weaveffi_kv_Store_destroy.argtypes = [ctypes.c_void_p]",
    );
    // Adoption, a second reference, and single disposal.
    assert_has(&py, "def _from_ptr(cls, ptr) -> \"Store\":");
    assert_has(&py, "return _lib.weaveffi_kv_Store_clone(_borrow(self))");
    assert_has(&py, "    def close(self) -> None:\n");
    assert_has(&py, "            _lib.weaveffi_kv_Store_destroy(_p)");
    assert_has(&py, "    def __del__(self) -> None:\n        self.close()");
    assert_has(
        &py,
        "    def __exit__(self, *exc) -> bool:\n        self.close()",
    );
    // The constructor adopts the returned reference; methods lend `self`.
    assert_has(&py, "def __init__(self, path: str) -> None:");
    assert_has(&py, "_fn = _lib.weaveffi_kv_Store_new");
    assert_has(&py, "self._ptr = _result");
    assert_has(
        &py,
        "_result = _fn(_borrow(self), _string_to_bytes(key), ctypes.byref(_err))",
    );
    assert_has(&py, "return _take_string(_result) or \"\"");
    // Stubs mirror the disposal surface.
    assert_has(&pyi, "    def close(self) -> None: ...\n");
    assert_has(&pyi, "    def __enter__(self) -> \"Store\": ...\n");
}

#[test]
fn nullable_objects_map_none_to_null_in_both_directions() {
    let (py, pyi) = render(&kv_api());
    assert_has(
        &py,
        "def pick(store: Optional[\"Store\"]) -> Optional[\"Store\"]:",
    );
    assert_has(
        &py,
        "_result = _fn((_borrow(store) if store is not None else None), ctypes.byref(_err))",
    );
    assert_has(
        &py,
        "        if _result is None:\n            return None\n        return Store._from_ptr(_result)",
    );
    // A non-nullable object return traps on null instead.
    assert_has(
        &py,
        "if _result is None:\n            raise WeaveFFIError(-1, \"null pointer\")",
    );
    assert_has(&py, "def sibling(self) -> Optional[\"Store\"]:");
    assert_has(
        &pyi,
        "def pick(store: Optional[\"Store\"]) -> Optional[\"Store\"]: ...",
    );
}

#[test]
fn objects_inside_buffers_are_cloned_on_write_and_adopted_on_read() {
    let (py, _) = render(&kv_api());
    // Record field: hand the wrapper to the writer (which clones it when the
    // buffer is finished), adopt the token on read.
    assert_has(&py, "_w.write_object(value.store)");
    assert_has(&py, "store=Store._from_ptr(_r.read_object()),");
    // List field.
    assert_has(
        &py,
        "for _e0 in value.mirrors:\n        _w.write_object(_e0)",
    );
    assert_has(
        &py,
        "mirrors=[Store._from_ptr(_r.read_object()) for _i0 in range(_r.read_len())],",
    );
    // Optional field keeps the ordinary flag byte.
    assert_has(
        &py,
        "backup=(Store._from_ptr(_r.read_object()) if _r.read_option_flag() else None),",
    );
    assert_has(&py, "def read_object(self) -> int:");
    assert_has(&py, "def write_object(self, obj) -> None:");
    // Tokens are minted in finish(), so a failed encoding leaks no reference.
    assert_has(
        &py,
        "struct.pack_into(\"<Q\", self._buf, _pos, _obj._clone_ptr())",
    );
}

#[test]
fn iterator_and_async_results_adopt_objects() {
    let (py, _) = render(&kv_api());
    assert_has(&py, "class _AllStoresIterator:");
    assert_has(&py, "_next_fn = _lib.weaveffi_kv_AllStoresIterator_next");
    assert_has(&py, "return Store._from_ptr(_out_item.value)");
    assert_has(
        &py,
        "_destroy_fn = _lib.weaveffi_kv_AllStoresIterator_destroy",
    );
    assert_has(&py, "async def fetch(id: int) -> \"Store\":");
    assert_has(&py, "_state[\"val\"] = Store._from_ptr(result)");
}

#[test]
fn callback_interfaces_render_abc_vtable_and_trampolines() {
    let (py, pyi) = render(&kv_api());
    assert_has(&py, "import abc\n");
    assert_has(&py, "_cb_impls: Dict[int, object] = {}");

    // The consumer-facing abstract base class.
    assert_has(&py, "class Watcher(abc.ABC):");
    assert_has(&py, "Observes store changes.");
    assert_has(
        &py,
        "    @abc.abstractmethod\n    def on_change(self, text: str, weight: int, entry: \"Entry\", store: \"Store\") -> None:",
    );
    assert_has(
        &py,
        "    @abc.abstractmethod\n    def should_stop(self, count: int) -> bool:",
    );

    // CFUNCTYPEs mirror the vtable entry signatures: ctx, slots, out_err.
    assert_has(
        &py,
        "_Watcher_on_change_cfunctype = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int32, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_void_p, ctypes.POINTER(_WeaveFFIErrorStruct))",
    );
    assert_has(
        &py,
        "_Watcher_should_stop_cfunctype = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_int32, ctypes.POINTER(_WeaveFFIErrorStruct))",
    );
    assert_has(
        &py,
        "_Watcher_vtable_free_cfunctype = ctypes.CFUNCTYPE(None, ctypes.c_void_p)",
    );

    // The vtable struct: methods in declaration order, then `free`.
    assert_has(&py, "class _WatcherVtable(ctypes.Structure):");
    assert_has(&py, "The C vtable `weaveffi_kv_Watcher_vtable`.");
    assert_has(
        &py,
        "        (\"on_change\", _Watcher_on_change_cfunctype),\n        (\"should_stop\", _Watcher_should_stop_cfunctype),\n        (\"free\", _Watcher_vtable_free_cfunctype),\n",
    );

    // Trampolines decode borrowed args, adopt objects, and never unwind.
    assert_has(
        &py,
        "def _Watcher_on_change_trampoline(ctx, text, weight, entry_ptr, entry_len, store, out_err):",
    );
    assert_has(&py, "_impl = _cb_impls[ctx]");
    assert_has(
        &py,
        "_impl.on_change(_bytes_to_string(text), weight, _unpack_Entry(ctypes.string_at(entry_ptr, entry_len) if entry_ptr else b\"\"), Store._from_ptr(store))",
    );
    assert_has(
        &py,
        "def _Watcher_should_stop_trampoline(ctx, count, out_err):",
    );
    assert_has(
        &py,
        "_ret = _impl.should_stop(count)\n        return 1 if _ret else 0",
    );
    assert_has(&py, "except Exception as _exc:");
    assert_has(
        &py,
        "_lib.weaveffi_error_set(out_err, -4, str(_exc).encode(\"utf-8\", \"replace\"))\n        return 0\n",
    );
    assert_has(
        &py,
        "def _Watcher_vtable_free_trampoline(ctx):\n    # The producer's last reference is gone; it never touches `ctx` again.\n    _cb_impls.pop(ctx, None)",
    );

    // Exactly one static vtable whose function objects are pinned.
    assert_has(
        &py,
        "_Watcher_on_change_cfunc = _Watcher_on_change_cfunctype(_Watcher_on_change_trampoline)",
    );
    assert_has(
        &py,
        "_Watcher_vtable = _WatcherVtable(_Watcher_on_change_cfunc, _Watcher_should_stop_cfunc, _Watcher_vtable_free_cfunc)",
    );
    assert_eq!(py.matches("_Watcher_vtable = _WatcherVtable(").count(), 1);

    // Passing an implementation: register, then ctx + vtable address.
    assert_has(&py, "def watch(watcher: \"Watcher\") -> None:");
    assert_has(
        &py,
        "_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER(_WatcherVtable), ctypes.POINTER(_WeaveFFIErrorStruct)]",
    );
    assert_has(&py, "_watcher_ctx = _cb_register(watcher)");
    assert_has(
        &py,
        "_fn(_watcher_ctx, ctypes.byref(_Watcher_vtable), ctypes.byref(_err))",
    );

    // Stubs.
    assert_has(&pyi, "from abc import ABC, abstractmethod\n");
    assert_has(&pyi, "class Watcher(ABC):");
    assert_has(
        &pyi,
        "    @abstractmethod\n    def on_change(self, text: str, weight: int, entry: \"Entry\", store: \"Store\") -> None: ...\n",
    );
    assert_has(&pyi, "def watch(watcher: \"Watcher\") -> None: ...");
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
    let api = validate_api(
        Api {
            version: CURRENT_SCHEMA_VERSION.into(),
            modules: vec![plain],
        },
        None,
    )
    .unwrap();
    let (py, _) = render(&api);
    assert!(!py.contains("import abc"));
    assert!(!py.contains("import asyncio"));
    assert!(!py.contains("_cb_impls"));
    assert_has(&py, "def add(a: int, b: int) -> int:");
}

#[test]
fn generated_source_is_deterministic() {
    let api = kv_api();
    assert_eq!(render(&api), render(&api));
}

#[test]
fn package_skips_platforms_without_a_wheel_tag() {
    let api = kv_api();
    let config = PythonConfig::default();
    let model = BindingModel::build(&api, config.prefix());
    let mut binaries = BinarySet::new("kv");
    for p in Platform::ALL {
        binaries.insert(
            p,
            format!("/tmp/{}/{}", p.id(), binaries.bundled_filename(p)),
        );
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("kv.yml"),
    };
    let files = PythonGenerator
        .package(&api, &model, &ctx, Utf8Path::new("out"), &config)
        .expect("python packages");
    let trees: std::collections::BTreeSet<&str> = files
        .iter()
        .filter_map(|f| f.path.as_str().strip_prefix("out/python/"))
        .filter_map(|rest| rest.split('/').next())
        .collect();
    let expected: std::collections::BTreeSet<&str> =
        Platform::DESKTOP.iter().map(|p| p.id()).collect();
    assert_eq!(trees, expected);
    for p in [
        Platform::AndroidArm64,
        Platform::AndroidX64,
        Platform::Wasm32,
    ] {
        assert!(!files.iter().any(|f| f.path.as_str().contains(p.id())));
    }
}
