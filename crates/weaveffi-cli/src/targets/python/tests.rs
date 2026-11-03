//! Unit tests of what's specific to the Python target: the package layout
//! and configuration, the identifier policy (keyword and builtin escaping,
//! exception and code class names), one C value per value, the embedded
//! contract, feature-gated runtime parts, range checks placed so a failure
//! strands nothing, and wheel packaging. The `kitchen_sink` snapshot pins
//! the rest of the generated module.

use crate::codegen::OutputFile;
use crate::package::{ArtifactKind, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::Target;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::targets::python::{PythonConfig, PythonGenerator};

const KV: &str = r#"
version: "0.13.0"
errors:
  - name: KvError
    codes:
      - { name: NotFound, code: 1, message: "not found", fields: [{ name: key, type: string }] }
      - { name: Timeout, code: 2, message: "timed out" }
records:
  - name: Entry
    fields:
      - { name: key, type: string }
      - { name: int, type: "[i32]" }
interfaces:
  - name: Store
    constructors:
      - { name: new, params: [{ name: path, type: string }] }
    methods:
      - { name: get, params: [{ name: key, type: string }], return: string, throws: KvError }
callbacks:
  - name: Watcher
    methods:
      - { name: on_change, params: [{ name: count, type: i32 }] }
functions:
  - { name: watch, params: [{ name: watcher, type: Watcher }, { name: limit, type: u8 }] }
  - { name: scale, params: [{ name: len, type: i16 }, { name: class, type: "u32?" }], return: i16 }
  - { name: all_stores, params: [], return: "iter<Store>" }
"#;

fn model(yaml: &str) -> Model {
    let api = parse_api_str(yaml, "yaml").expect("fixture parses");
    validate(&api, &Identity::named("kv-lib"), None)
        .unwrap_or_else(|d| panic!("fixture must validate: {d:?}"))
}

fn render_files(model: &Model, config: &PythonConfig) -> Vec<OutputFile> {
    PythonGenerator::from(config.clone()).render(model)
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
            "kv_lib/__init__.py",
            "kv_lib/kv_lib.py",
            "kv_lib/py.typed",
            "pyproject.toml",
            "README.md",
        ]
    );
    assert_has(&files[0].contents, "from .kv_lib import *");
    let pyproject = &files[3].contents;
    assert_has(pyproject, "name = \"kv-lib\"");
    assert_has(pyproject, "requires-python = \">=3.10\"");
    assert_has(pyproject, "packages = [\"kv_lib\"]");
    assert_has(pyproject, "\"kv_lib\" = [\"py.typed\", \"*.so\"");
    assert_has(&files[4].contents, "- Python 3.10 or later");

    let config = PythonConfig {
        name: Some("kv-python".into()),
        import_name: Some("kvpy".into()),
        requires_python: ">=3.12".into(),
    };
    let files = render_files(&model(KV), &config);
    assert!(files.iter().any(|f| f.path == "kvpy/kvpy.py"));
    let pyproject = files.iter().find(|f| f.path.ends_with("pyproject.toml"));
    assert_has(&pyproject.unwrap().contents, "name = \"kv-python\"");
    assert_has(&pyproject.unwrap().contents, "requires-python = \">=3.12\"");
    let dir = PythonGenerator::from(config).dev_bundle_dir(&model(KV));
    assert_eq!(dir.as_deref().map(|d| d.as_str()), Some("kvpy"));
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
            "LibraryLoadError",
            "NativeIterator",
            "KvError",
            "NotFoundError",
            "KvTimeoutError",
            "Entry",
            "Watcher",
            "Store",
            "watch",
            "scale",
            "all_stores",
        ]
    );
}

#[test]
fn every_declaration_is_checked_by_its_contract_function() {
    let model = model(KV);
    let py = render(&model);
    assert_has(&py, "_ABI_VERSION = 6\n");
    for row in crate::codegen::contract::rows(&model) {
        assert_has(
            &py,
            &format!(
                "    (\"{}\", 0x{:016x}, \"{}\"),  # {}\n",
                row.symbol, row.hash, row.path, row.signature
            ),
        );
    }
    assert_has(
        &py,
        "\"Store.get\"),  # method get(string) -> string throws KvError",
    );
    assert_has(
        &py,
        "raise _refuse(f\"{path} is missing from the library\") from None",
    );
    assert!(!py.contains("_CONTRACTS"), "no per-module tables");
}

#[test]
fn every_value_is_one_c_value() {
    let py = render(&model(KV));
    // An optional scalar is one `_Opt{Prim}` structure; a string is one run.
    assert_has(
        &py,
        "_c_scale = _bind(\"kv_lib_scale\", ctypes.c_int16, ctypes.c_int16, _OptU32, \
         ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(
        &py,
        "_c_Store_get = _bind(\"kv_lib_Store_get\", _Run, ctypes.c_void_p, _Run, \
         ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(&py, "return _take_str(_ret)");
    // A callback interface is one `{ ctx, vtable }` structure by value.
    assert_has(
        &py,
        "_c_watch = _bind(\"kv_lib_watch\", None, _WatcherArg, ctypes.c_uint8, \
         ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(
        &py,
        "_c_watch(_WatcherArg(_watcher_ctx, _watcher_vt), _limit_v, ",
    );
    // An iterator's item is one out slot of its C type.
    assert_has(
        &py,
        "_c_AllStoresIterator_next = _bind(\"kv_lib_AllStoresIterator_next\", ctypes.c_int32, \
         ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(_ErrorStruct))",
    );
    assert_has(&py, "return Store._adopt(_required(_item.value))");
    for gone in ["out_len", "free_bytes", "free_string", "has_"] {
        assert!(!py.contains(gone), "ABI 5 slot `{gone}` in:\n{py}");
    }
}

#[test]
fn code_classes_never_shadow_builtins_or_declarations() {
    let py = render(&model(KV));
    // `Timeout` would be `TimeoutError`, Python's builtin: the domain's stem
    // qualifies it. The scoped alias keeps the bare code name.
    assert_has(&py, "class NotFoundError(KvError):");
    assert_has(&py, "class KvTimeoutError(KvError):");
    assert_has(&py, "KvError.Timeout = KvTimeoutError");
    assert!(!py.contains("class TimeoutError"));

    let py = render(&model(
        r#"
version: "0.13.0"
errors:
  - name: Failure
    codes: [{ name: Broken, code: 1, message: broken }]
  - name: Other
    codes: [{ name: Full, code: 1, message: full }]
records:
  - name: BrokenError
    fields: [{ name: code, type: i32 }]
"#,
    ));
    assert_has(&py, "class FailureError(Error):");
    assert_has(&py, "class FailureBrokenError(FailureError):");
    assert_has(&py, "class FullError(OtherError):");
}

#[test]
fn exception_names_avoid_user_type_names() {
    let py = render(&model(
        r#"
version: "0.13.0"
records:
  - name: Error
    fields: [{ name: code, type: i32 }]
  - name: InternalError
    fields: [{ name: code, type: i32 }]
"#,
    ));
    assert_has(&py, "class KvLibError(Exception):");
    assert_has(&py, "class KvLibInternalError(RuntimeError):");
    assert_has(&py, "def _kv_lib_error_from(");
}

#[test]
fn names_escape_keywords_and_builtins_the_body_uses() {
    let py = render(&model(KV));
    // A parameter named like a keyword or a builtin the body calls gets `_`.
    assert_has(&py, "def scale(len_: int, class_: int | None) -> int:");
    assert_has(&py, "_i16(len_, \"len_\")");
    // A field named like a builtin type spells the builtins in its class.
    assert_has(&py, "    int: builtins.list[builtins.int]\n");
}

#[test]
fn range_checks_never_strand_a_lent_object_or_registration() {
    let py = render(&model(KV));
    // Nothing lent: the checks run in the argument list.
    assert_has(
        &py,
        "_ret: int = _c_scale(_i16(len_, \"len_\"), _OptU32(True, _u32(class_, \"class_\")) \
         if class_ is not None else _OptU32(), ctypes.byref(_err))",
    );
    // A callback is checked first and registered just before the call, so
    // every other check runs before it.
    assert_has(
        &py,
        "    _watcher_vt = _callback_vtable(watcher, Watcher, _WatcherVtableOnce)\n    \
         _limit_v = _u8(limit, \"limit\")\n    _err = _ErrorStruct()\n    \
         _watcher_ctx = _callback_register(watcher)\n",
    );
}

#[test]
fn struct_returning_callback_methods_are_libffi_closures() {
    let py = render(&model(KV));
    assert_has(
        &py,
        "_vtable_entry(_Watcher_on_change, None, ctypes.c_void_p, ctypes.c_int32, \
         ctypes.c_void_p),",
    );
    assert!(
        !py.contains("_vtable_struct_entry"),
        "no libffi runtime without a need"
    );

    let py = render(&model(
        r#"
version: "0.13.0"
callbacks:
  - name: Source
    methods:
      - { name: name, return: string }
      - { name: limit, params: [{ name: level, type: "u32?" }], return: "f64?" }
      - { name: count, return: u64 }
functions:
  - { name: drain, params: [{ name: source, type: Source }] }
"#,
    ));
    assert_has(&py, "def _vtable_struct_entry(");
    assert_has(
        &py,
        "_vtable_struct_entry(_Source_name, _Run, ctypes.c_void_p, ctypes.c_void_p),",
    );
    assert_has(
        &py,
        "_vtable_struct_entry(_Source_limit, _OptF64, ctypes.c_void_p, _OptU32, ctypes.c_void_p),",
    );
    assert_has(
        &py,
        "_vtable_entry(_Source_count, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_void_p),",
    );
    // The trampolines take and return the one C value.
    assert_has(
        &py,
        "def _Source_name(ctx: int | None, out_err: int | None) -> _Run:",
    );
    assert_has(&py, "        return _alloc_run(_ret.encode(\"utf-8\"))\n");
    assert_has(
        &py,
        "def _Source_limit(ctx: int | None, level: _OptU32, out_err: int | None) -> _OptF64:",
    );
    assert_has(&py, "_callback_get(ctx).limit(_opt_int(level))");
    assert_has(
        &py,
        "return _OptF64(True, _float(_ret, \"Source.limit() result\")) if _ret is not None \
         else _OptF64()",
    );
    assert_has(&py, "        return _OptF64()\n");
}

#[test]
fn feature_runtime_is_emitted_only_when_used() {
    let py = render(&model(
        r#"
version: "0.13.0"
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
    assert!(!py.contains("_FfiType"));
    assert!(!py.contains("Value-buffer codecs of composite types"));
    assert_has(&py, "def add(a: int, b: int) -> int:");
    // Nothing but the generated-file header names WeaveFFI.
    for line in py.lines().filter(|l| !l.starts_with('#')) {
        assert!(
            !line.to_ascii_lowercase().contains("weaveffi"),
            "branded line: {line}"
        );
    }
}

#[test]
fn async_results_are_one_completion_argument() {
    let py = render(&model(
        r#"
version: "0.13.0"
functions:
  - { name: fetch, params: [{ name: key, type: string }], return: "i64?", async: true, cancellable: true }
"#,
    ));
    assert_has(
        &py,
        "_c_fetch_callback = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p, _OptI64)",
    );
    assert_has(
        &py,
        "_c_fetch = _bind(\"kv_lib_fetch\", None, _Run, ctypes.c_void_p, _c_fetch_callback, \
         ctypes.c_void_p)",
    );
    assert_has(
        &py,
        "def _fetch_complete(context: int | None, err: int | None, result: _OptI64) -> None:",
    );
    assert_has(&py, "_async_settle(context, None, _opt_int(result))");
    assert_has(
        &py,
        "_c_fetch(_run_in(key.encode(\"utf-8\")), _token, _fetch_completion, _call)",
    );
    assert_has(
        &py,
        "_result: int | None = await _async_wait_cancellable(_future, _token)",
    );
}

/// A 64-bit little-endian ELF shared-object header with no sections.
fn minimal_elf() -> Vec<u8> {
    let mut elf = vec![0u8; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2; // ELFCLASS64
    elf[5] = 1; // ELFDATA2LSB
    elf[6] = 1; // EV_CURRENT
    elf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
    elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
    elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf
}

#[test]
fn package_bundles_the_identity_library_per_wheel_platform() {
    let model = model(KV);
    // The manylinux tag is read from a Linux library's ELF version needs, so
    // those must be real ELF files (a bare header needs nothing, giving the
    // 2.17 floor); the others are never read.
    let dir = tempfile::tempdir().unwrap();
    let elf = dir.path().join("libkv_lib.so");
    std::fs::write(&elf, minimal_elf()).unwrap();
    let elf = camino::Utf8PathBuf::from_path_buf(elf).unwrap();
    let mut binaries = BinarySet::new("kv_lib");
    for p in Platform::ALL {
        let path = match p.os() {
            crate::platform::Os::Linux => elf.clone(),
            _ => format!("/tmp/{}/lib", p.id()).into(),
        };
        binaries.insert(NativeBinary::new(p, path));
    }
    let mut ctx = PackageContext::new(&binaries);
    ctx.macos_deployment_target = "12.0";
    let artifacts = PythonGenerator::from(PythonConfig::default())
        .package(&model, &ctx)
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
    assert_eq!(meta.requires_python.as_deref(), Some(">=3.10"));
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
version: "0.13.0"
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
