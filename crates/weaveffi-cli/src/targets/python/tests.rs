//! Unit tests: render a small API that exercises every ABI revision 4 shape
//! (prefixed runtime, contract table, pointer + length strings,
//! reference-counted objects, nullable objects, objects inside buffers,
//! composite codecs, typed errors with payloads, iterators, async and
//! cancellable functions, and callback interfaces with every return family,
//! a throwing method, and an optional parameter) and assert the generated
//! Python carries the pieces each contract clause requires.

use crate::backend::{LanguageBackend, OutputFile};
use crate::package::{ArtifactKind, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use camino::Utf8Path;
use weaveffi_model::contract;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::targets::python::{PythonConfig, PythonGenerator};

const KV: &str = r#"
version: "0.11.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - { name: NotFound, code: 1, message: "not found", fields: [{ name: key, type: string }] }
        - { name: Busy, code: 2, message: "busy" }
    structs:
      - name: Entry
        fields:
          - { name: key, type: string }
          - { name: store, type: Store }
          - { name: mirrors, type: "[Store]" }
          - { name: weights, type: "[i32]" }
          - { name: labels, type: "{string:i64?}" }
    interfaces:
      - name: Store
        doc: A key-value store.
        constructors:
          - { name: new, params: [{ name: path, type: string }] }
        methods:
          - { name: get, params: [{ name: key, type: string }], return: string, throws: true }
          - { name: has, params: [{ name: key, type: string }], return: bool }
          - { name: size, params: [], return: u32, deprecated: "use count" }
    callback_interfaces:
      - name: Watcher
        doc: Observes store changes.
        methods:
          - name: on_change
            params:
              - { name: text, type: string }
              - { name: entry, type: Entry }
              - { name: store, type: Store }
          - { name: should_stop, params: [{ name: count, type: i32 }], return: bool }
          - { name: label, params: [], return: string, throws: true }
          - { name: snapshot, params: [], return: Entry }
          - { name: pick, params: [{ name: store, type: "Store?" }], return: "Store?" }
          - { name: home, params: [], return: Store }
    functions:
      - name: pick
        params: [{ name: store, type: "Store?" }]
        return: "Store?"
      - { name: watch, params: [{ name: watcher, type: Watcher }] }
      - { name: maybe_watch, params: [{ name: watcher, type: "Watcher?" }] }
      - { name: all_stores, params: [], return: "iter<Store>" }
      - { name: fetch, params: [{ name: id, type: i64 }], return: Store, async: true }
      - name: sleep
        params: [{ name: ms, type: i64 }]
        return: string
        async: true
        cancellable: true
        throws: true
"#;

fn model(yaml: &str) -> Model {
    let api = parse_api_str(yaml, "yaml").expect("fixture parses");
    validate(&api, &Identity::named("kv-lib"), None)
        .unwrap_or_else(|d| panic!("fixture must validate: {d:?}"))
}

fn render_files(model: &Model, config: &PythonConfig) -> Vec<OutputFile> {
    PythonGenerator.files(model, Utf8Path::new("out"), config)
}

/// Render `model` and return the implementation module.
fn render(model: &Model) -> String {
    render_files(model, &PythonConfig::default())
        .into_iter()
        .find(|f| f.path.as_str().ends_with("kv_lib.py"))
        .expect("kv_lib.py")
        .contents
}

fn assert_has(hay: &str, needle: &str) {
    assert!(hay.contains(needle), "missing `{needle}` in:\n{hay}");
}

#[test]
fn layout_follows_the_identity() {
    let files = render_files(&model(KV), &PythonConfig::default());
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/python/kv_lib/__init__.py",
            "out/python/kv_lib/kv_lib.py",
            "out/python/kv_lib/py.typed",
            "out/python/pyproject.toml",
            "out/python/README.md",
        ]
    );
    assert_has(&files[0].contents, "from .kv_lib import *");
    let pyproject = &files[3].contents;
    assert_has(pyproject, "name = \"kv-lib\"");
    assert_has(pyproject, "requires-python = \">=3.9\"");
    assert_has(pyproject, "packages = [\"kv_lib\"]");
    assert_has(pyproject, "\"kv_lib\" = [\"py.typed\", \"*.so\"");

    let config = PythonConfig {
        name: Some("kv-python".into()),
        import_name: Some("kvpy".into()),
        ..PythonConfig::default()
    };
    let files = render_files(&model(KV), &config);
    assert!(files.iter().any(|f| f.path == "out/python/kvpy/kvpy.py"));
    let pyproject = files.iter().find(|f| f.path.ends_with("pyproject.toml"));
    assert_has(&pyproject.unwrap().contents, "name = \"kv-python\"");
}

#[test]
fn runtime_checks_the_revision_and_every_contract_entry() {
    let m = model(KV);
    let py = render(&m);
    assert_has(&py, "from __future__ import annotations\n");
    assert_has(&py, "_ABI_VERSION = 4\n");
    assert_has(&py, "    \"kv_lib_kv_contract\": [\n");
    let root = m.roots().next().unwrap();
    for e in contract::entries(&m, root) {
        assert_has(
            &py,
            &format!("({:#018x}, {:#018x}, \"{}\"),", e.id, e.hash, e.path),
        );
    }
    assert_has(
        &py,
        "raise ImportError(f\"{path} is missing from the library",
    );
    assert_has(&py, "f\"{path} changed since these bindings were generated");
    assert!(!py.contains("checksum"), "no trace of the checksum check");
    assert_has(&py, "_bind(\"kv_lib_abi_version\", ctypes.c_uint32)");
    assert_has(&py, "os.environ.get(\"KV_LIB_LIBRARY\")");
    assert_has(&py, "name = \"libkv_lib.dylib\"");
    assert_has(
        &py,
        "_bind(\"kv_lib_free_bytes\", None, ctypes.c_void_p, ctypes.c_size_t)",
    );
    assert_has(
        &py,
        "_bind(\"kv_lib_alloc\", ctypes.c_void_p, ctypes.c_size_t)",
    );
    assert_has(&py, "\"kv_lib_error_set_payload\",");
    assert!(!py.contains("dealloc"));
    assert_has(&py, "class Error(Exception):");
    assert_has(&py, "class InternalError(RuntimeError):");
    // Nothing but the generated-file header names WeaveFFI.
    for line in py.lines().filter(|l| !l.starts_with('#')) {
        assert!(
            !line.to_ascii_lowercase().contains("weaveffi"),
            "branded line: {line}"
        );
    }
}

#[test]
fn failures_raise_the_domain_or_the_unchecked_trap() {
    let py = render(&model(KV));
    // A throwing call raises its domain; any other failure traps.
    assert_has(
        &py,
        "        if _err.code:\n            raise _kv_error_from(*_read_error(_err))\n        \
         return _take_str(_ret, _out_len.value)",
    );
    assert_has(
        &py,
        "        if _err.code:\n            raise _trap_from(*_read_error(_err))\n        \
         return _ret",
    );
    // Codes carry their payload fields as constructor arguments.
    assert_has(&py, "class NotFound(KvError):");
    assert_has(
        &py,
        "    def __init__(self, key: str, message: str = \"not found\") -> None:",
    );
    assert_has(
        &py,
        "        return _decode(payload, lambda _r: NotFound(\n            \
         key=_r.read_string(),\n            message=message,\n        ))",
    );
    assert_has(&py, "    def _payload(self) -> bytes:");
    assert_has(&py, "KvError.NotFound = NotFound");
}

#[test]
fn prototypes_are_bound_once_with_c_bool_and_ptr_len_strings() {
    let py = render(&model(KV));
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
    // A direct result is annotated for checkers.
    assert_has(&py, "_ret: bool = _c_kv_Store_has(");
    // Deprecation warns and is documented.
    assert_has(
        &py,
        "warnings.warn(\"use count\", DeprecationWarning, stacklevel=2)",
    );
    assert_has(&py, "Deprecated: use count");
}

#[test]
fn objects_are_lent_for_calls_and_adopted_from_returns() {
    let py = render(&model(KV));
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
    assert_has(&py, "return Store._adopt(_ret) if _ret else None");
}

#[test]
fn composites_get_one_codec_function_each() {
    let py = render(&model(KV));
    // Objects inside buffers are cloned when the buffer is finished and
    // adopted on read.
    assert_has(&py, "_w.write_object(value.store, Store)");
    assert_has(&py, "store=Store._adopt(_r.read_object()),");
    // Record fields delegate to one function per composite type.
    assert_has(&py, "    _write_list_Store(_w, value.mirrors)\n");
    assert_has(&py, "        mirrors=_read_list_Store(_r),\n");
    assert_eq!(py.matches("def _write_list_Store(").count(), 1);
    assert_eq!(py.matches("def _read_list_Store(").count(), 1);
    assert_has(
        &py,
        "def _read_list_Store(_r: _Reader) -> List[Store]:\n    \
         return [Store._adopt(_r.read_object()) for _ in range(_r.read_count())]",
    );
    // Numeric lists take the packed fast path.
    assert_has(&py, "    _w.write_numbers(\"i\", value)\n");
    assert_has(&py, "    return _r.read_numbers(\"i\", 4)\n");
    // Maps reject a repeated key; nested composites call each other.
    assert_has(
        &py,
        "def _read_map_string_opt_i64(_r: _Reader) -> Dict[str, Optional[int]]:\n    \
         n = _r.read_count()\n    \
         value = {_r.read_string(): _read_opt_i64(_r) for _ in range(n)}",
    );
    assert_has(&py, "raise _malformed(\"repeated map key\")");
    assert!(!py.contains("for _e0 in"), "no loop inlined at a call site");
}

#[test]
fn iterators_pull_lazily_through_prebound_functions() {
    let py = render(&model(KV));
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
    assert_has(&py, "def all_stores() -> Iterator[Store]:");
}

#[test]
fn async_calls_complete_through_static_trampolines() {
    let py = render(&model(KV));
    assert_has(&py, "import asyncio\n");
    assert_has(
        &py,
        "_c_kv_fetch_callback = ctypes.CFUNCTYPE(None, ctypes.c_void_p, \
         ctypes.POINTER(_ErrorStruct), ctypes.c_void_p)",
    );
    assert_has(
        &py,
        "def _kv_fetch_complete(context: Optional[int], err: Any, result: Optional[int]) -> None:",
    );
    assert_has(
        &py,
        "_async_settle(context, _async_error(err, _trap_from), None)",
    );
    assert_has(
        &py,
        "_async_settle(context, None, Store._adopt(_required(result)))",
    );
    assert_has(
        &py,
        "_kv_fetch_completion = _c_kv_fetch_callback(_kv_fetch_complete)",
    );
    assert_has(&py, "_c_kv_fetch(id, _kv_fetch_completion, _call)");
    assert_has(
        &py,
        "    _result: Store = await _future\n    return _result\n",
    );
    // A cancellable call owns a token for its duration.
    assert_has(&py, "_token = _cancel_token_create()");
    assert_has(&py, "_c_kv_sleep(ms, _token, _kv_sleep_completion, _call)");
    assert_has(
        &py,
        "_result: str = await _async_wait_cancellable(_future, _token)",
    );
    assert_has(
        &py,
        "_async_settle(context, _async_error(err, _kv_error_from), None)",
    );
    assert_has(
        &py,
        "_async_settle(context, None, _take_str(result_ptr, result_len))",
    );
    assert_has(&py, "async def sleep(ms: int) -> str:");
}

#[test]
fn callback_vtables_lead_with_the_header() {
    let py = render(&model(KV));
    assert_has(&py, "import abc\n");
    assert_has(&py, "class Watcher(abc.ABC):");
    assert_has(&py, "class _WatcherVtable(ctypes.Structure):");
    assert_has(&py, "The C vtable `kv_lib_kv_Watcher_vtable`.");
    assert_has(
        &py,
        "        (\"size\", ctypes.c_uint32),\n        (\"flags\", ctypes.c_uint32),\n        \
         (\"free\", _CallbackFree),\n        (\"on_change\", _Watcher_on_change_t),",
    );
    assert_has(
        &py,
        "_Watcher_vtable = _WatcherVtable(\n    ctypes.sizeof(_WatcherVtable),\n    0,\n    \
         _callback_free_fn,\n    _Watcher_on_change_t(_Watcher_on_change),",
    );
    assert_eq!(py.matches("_Watcher_vtable = _WatcherVtable(").count(), 1);
    assert_has(
        &py,
        "_Watcher_vtable_ptr = ctypes.addressof(_Watcher_vtable)",
    );
    assert_has(
        &py,
        "_Watcher_on_change_t = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, \
         ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, \
         ctypes.POINTER(_ErrorStruct))",
    );
}

#[test]
fn callback_trampolines_adopt_objects_and_return_every_family() {
    let py = render(&model(KV));
    // Object parameters are adopted before anything else can fail.
    assert_has(
        &py,
        "        _store = Store._adopt(_required(store))\n        \
         _callback_get(ctx).on_change(_peek_bytes(text_ptr, text_len).decode(\"utf-8\"), \
         _decode(_peek_bytes(entry_ptr, entry_len), _read_Entry), _store)",
    );
    // Direct returns by value, with a default after a failure.
    assert_has(&py, "return bool(_callback_get(ctx).should_stop(count))");
    assert_has(&py, "_callback_fail(out_err, exc)\n        return False");
    // Strings and buffers through the out slots, allocated for the producer.
    assert_has(
        &py,
        "def _Watcher_label(ctx: Optional[int], out_ptr: Any, out_len: Any, out_err: Any) -> None:",
    );
    assert_has(
        &py,
        "_callback_return_bytes(out_ptr, out_len, _ret.encode(\"utf-8\"))",
    );
    assert_has(
        &py,
        "        _w = _Writer()\n        _write_Entry(_w, _ret)\n        \
         _callback_return_bytes(out_ptr, out_len, _w.finish())",
    );
    // Objects as a fresh reference; `I?` may be None.
    assert_has(&py, "return _callback_return_object(_ret, Store)");
    assert_has(&py, "return _callback_return_object_opt(_ret, Store)");
    assert_has(&py, "_store = Store._adopt(store) if store else None");
    // A throwing method reports its domain's codes; others report -4.
    assert_has(&py, "_callback_fail(out_err, exc, KvError)");
    assert_eq!(
        py.matches("_callback_fail(out_err, exc, KvError)").count(),
        1
    );
}

#[test]
fn callback_parameters_register_and_none_passes_a_null_vtable() {
    let py = render(&model(KV));
    assert_has(&py, "_watcher_ctx = _callback_register(watcher, Watcher)");
    assert_has(
        &py,
        "_c_kv_watch(_watcher_ctx, _Watcher_vtable_ptr, ctypes.byref(_err))",
    );
    assert_has(&py, "def maybe_watch(watcher: Optional[Watcher]) -> None:");
    assert_has(
        &py,
        "_watcher_ctx = _callback_register_opt(watcher, Watcher)",
    );
    assert_has(
        &py,
        "_watcher_ctx, _Watcher_vtable_ptr if watcher is not None else None,",
    );
}

#[test]
fn public_names_are_exported_and_nothing_else() {
    let py = render(&model(KV));
    let all = py
        .split("__all__ = [\n")
        .nth(1)
        .and_then(|rest| rest.split("]\n").next())
        .expect("__all__");
    let names: Vec<&str> = all
        .lines()
        .map(|l| l.trim().trim_end_matches(',').trim_matches('"'))
        .collect();
    assert_eq!(
        names,
        [
            "Error",
            "InternalError",
            "KvError",
            "NotFound",
            "Busy",
            "Entry",
            "Watcher",
            "Store",
            "pick",
            "watch",
            "maybe_watch",
            "all_stores",
            "fetch",
            "sleep",
        ]
    );
}

#[test]
fn feature_runtime_is_emitted_only_when_used() {
    let py = render(&model(
        r#"
version: "0.11.0"
modules:
  - name: math
    functions:
      - name: add
        params: [{ name: a, type: i32 }, { name: b, type: i32 }]
        return: i32
"#,
    ));
    assert!(!py.contains("import abc"));
    assert!(!py.contains("import asyncio"));
    assert!(!py.contains("_callbacks"));
    assert!(!py.contains("_cancel_token"));
    assert!(!py.contains("Value-buffer codecs of composite types"));
    assert_has(&py, "def add(a: int, b: int) -> int:");
}

#[test]
fn exception_names_avoid_user_type_names() {
    let py = render(&model(
        r#"
version: "0.11.0"
modules:
  - name: kv
    structs:
      - name: Error
        fields: [{ name: code, type: i32 }]
      - name: InternalError
        fields: [{ name: code, type: i32 }]
"#,
    ));
    assert_has(&py, "class KvLibError(Exception):");
    assert_has(&py, "class KvLibInternalError(RuntimeError):");
}

#[test]
fn generated_source_is_deterministic() {
    let model = model(KV);
    assert_eq!(render(&model), render(&model));
}

#[test]
fn package_bundles_the_identity_library_per_wheel_platform() {
    let model = model(KV);
    let mut binaries = BinarySet::new("kv_lib");
    for p in Platform::ALL {
        binaries.insert(NativeBinary::new(p, format!("/tmp/{}/lib", p.id())));
    }
    let mut ctx = PackageContext::new(&binaries);
    ctx.macos_deployment_target = "12.0";
    let artifacts = PythonGenerator
        .package(&model, &ctx, &PythonConfig::default())
        .expect("python packages");
    let wheels: Vec<&str> = artifacts.iter().map(|a| a.path.as_str()).collect();
    assert_eq!(
        wheels,
        [
            "python/kv_lib-0.1.0-py3-none-macosx_12_0_arm64.whl",
            "python/kv_lib-0.1.0-py3-none-macosx_12_0_x86_64.whl",
            "python/kv_lib-0.1.0-py3-none-manylinux_2_17_x86_64.whl",
            "python/kv_lib-0.1.0-py3-none-manylinux_2_17_aarch64.whl",
            "python/kv_lib-0.1.0-py3-none-win_amd64.whl",
        ]
    );
    let mac = &artifacts[0];
    let ArtifactKind::Wheel(meta) = &mac.kind else {
        panic!("a wheel");
    };
    assert_eq!(meta.name, "kv-lib");
    assert_eq!(meta.requires_python.as_deref(), Some(">=3.9"));
    let names: Vec<&str> = mac.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        names,
        [
            "kv_lib/__init__.py",
            "kv_lib/kv_lib.py",
            "kv_lib/py.typed",
            "kv_lib/libkv_lib.dylib",
        ]
    );
    assert!(artifacts[4].file("kv_lib/kv_lib.dll").is_some());
}

#[test]
fn members_never_replace_the_wrapper_close() {
    let out = render(&model(
        r#"
version: "0.11.0"
modules:
  - name: kv
    interfaces:
      - name: Store
        constructors:
          - { name: close, params: [] }
        methods:
          - { name: close_all, params: [] }
        statics:
          - { name: closed, params: [], return: bool }
      - name: Lock
        methods:
          - { name: close, params: [], return: bool }
"#,
    ));
    assert_has(&out, "    def close_(cls) -> Store:");
    assert_has(&out, "    def close_(self) -> bool:");
    assert_has(&out, "    def close_all(self) -> None:");
    assert_has(&out, "    def closed() -> bool:");
    // The runtime's `_Handle.close` is the only `close`.
    assert_eq!(
        out.matches("def close(").count(),
        1,
        "a member replaced close"
    );
}
