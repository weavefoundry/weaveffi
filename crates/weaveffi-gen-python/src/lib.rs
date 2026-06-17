//! Python (`ctypes`) binding generator for WeaveFFI.
//!
//! Emits a pip-installable package containing `ctypes`-based bindings and
//! `.pyi` type stubs over the C ABI. Async functions surface as
//! `async def` wrappers. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::ToSnakeCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, is_c_pointer_type, pascal_case, DocCommentStyle,
};
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, FieldBinding, FnBinding,
    ListenerBinding, ModuleBinding, ParamBinding, RichVariantBinding, StructBinding,
};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`PythonGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PythonConfig {
    /// pip-installable Python package name (default `"weaveffi"`). Also
    /// determines the on-disk package directory inside `python/`.
    pub package_name: Option<String>,
    /// When `true`, strip the IR module name prefix from emitted Python
    /// function names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the ctypes bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl PythonConfig {
    /// Returns the configured Python package name, falling back to `"weaveffi"`.
    pub fn package_name(&self) -> &str {
        self.package_name.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Python backend: emits a pip-installable package of `ctypes` bindings and
/// `.pyi` type stubs over the C ABI exposed by the underlying cdylib.
pub struct PythonGenerator;

impl PythonGenerator {
    /// Render the primary `weaveffi.py` source by composing the shared
    /// [`LanguageBackend::emit_members`] walk over every module. Shared by the
    /// [`LanguageBackend::files`] hook and the test-facing
    /// [`render_python_module`] wrapper so there is one assembly path.
    fn render_py_source(
        &self,
        model: &BindingModel,
        strip_module_prefix: bool,
        input_basename: &str,
    ) -> String {
        let config = PythonConfig {
            strip_module_prefix,
            ..PythonConfig::default()
        };
        let mut out = render_prelude(CommentStyle::Hash, input_basename);
        render_preamble(&mut out);
        let has_async = model.functions().any(|(_, f)| f.is_async);
        if has_async {
            out.push_str("\nimport asyncio\nimport threading\n");
        }
        let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
        if has_listeners {
            out.push_str(
                "\n\n# Registered listener trampolines, keyed by subscription id. Holding\n\
                 # the ctypes function objects here keeps them alive until unregistered;\n\
                 # without this the GC could collect a trampoline the producer still calls.\n\
                 _listener_refs: Dict[int, object] = {}\n",
            );
        }
        // The model is a flat, pre-order list of modules, each carrying its
        // joined symbol path, the same traversal order the recursive walk
        // produced.
        for m in &model.modules {
            out.push_str(&format!("\n\n# === Module: {} ===", m.path));
            self.emit_members(&mut out, m, &config);
        }
        out.push('\n');
        out.push_str(&render_trailer(CommentStyle::Hash, "weaveffi.py"));
        out
    }
}

impl LanguageBackend for PythonGenerator {
    type Config = PythonConfig;

    fn name(&self) -> &'static str {
        "python"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn render_enum(&self, out: &mut String, e: &EnumBinding, _config: &Self::Config) {
        render_enum(out, e);
    }

    fn render_struct(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        s: &StructBinding,
        _config: &Self::Config,
    ) {
        render_struct(out, s);
        if s.builder.is_some() {
            render_builder(out, s);
        }
    }

    fn render_callback(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        c: &CallbackBinding,
        _config: &Self::Config,
    ) {
        render_callback_type(out, c);
    }

    fn render_listener(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        l: &ListenerBinding,
        config: &Self::Config,
    ) {
        render_listener(out, module, l, config.strip_module_prefix);
    }

    fn render_function(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        f: &FnBinding,
        config: &Self::Config,
    ) {
        render_function(out, &module.path, f, config.strip_module_prefix);
    }

    fn files(
        &self,
        api: &Api,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let import_name = package.ident_name();
        let input_basename = config.input_basename();
        let dir = out_dir.join("python");
        let pkg_dir = dir.join(&import_name);
        let hash = CommentStyle::Hash;
        vec![
            OutputFile::new(
                pkg_dir.join("__init__.py"),
                format!(
                    "{}from .weaveffi import *  # noqa: F401,F403\n\n{}",
                    render_prelude(hash, input_basename),
                    render_trailer(hash, "__init__.py"),
                ),
            ),
            OutputFile::new(
                pkg_dir.join("weaveffi.py"),
                self.render_py_source(model, config.strip_module_prefix, input_basename),
            ),
            OutputFile::new(
                pkg_dir.join("weaveffi.pyi"),
                render_pyi_module(api, config.strip_module_prefix, input_basename),
            ),
            OutputFile::new(
                dir.join("pyproject.toml"),
                render_pyproject_toml(&package, &import_name, input_basename),
            ),
            OutputFile::new(
                dir.join("setup.py"),
                render_setup_py(&package, &import_name, input_basename),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&package, input_basename),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(PythonGenerator);

// ── Type helpers ──

fn py_ctypes_scalar(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "ctypes.c_int8",
        TypeRef::I16 => "ctypes.c_int16",
        TypeRef::I32 => "ctypes.c_int32",
        TypeRef::U8 => "ctypes.c_uint8",
        TypeRef::U16 => "ctypes.c_uint16",
        TypeRef::U32 => "ctypes.c_uint32",
        TypeRef::I64 => "ctypes.c_int64",
        TypeRef::U64 => "ctypes.c_uint64",
        TypeRef::F32 => "ctypes.c_float",
        TypeRef::F64 => "ctypes.c_double",
        TypeRef::Bool => "ctypes.c_int32",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "ctypes.c_char_p",
        TypeRef::Handle => "ctypes.c_uint64",
        TypeRef::TypedHandle(_) => "ctypes.c_void_p",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ctypes.c_uint8",
        TypeRef::Struct(_) => "ctypes.c_void_p",
        TypeRef::Enum(_) => "ctypes.c_int32",
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) | TypeRef::Iterator(_) => {
            "ctypes.c_void_p"
        }
    }
}

fn py_type_hint(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::Handle => "int".into(),
        // Structs, enums, and typed handles all surface as bare local class names
        // in the generated module. A cross-module reference (e.g. `handle<Store>`
        // resolved to `kv.Store`) must still annotate the *local* `Store`, not the
        // qualified IR name, which is not a symbol in this module.
        TypeRef::TypedHandle(name) => format!("\"{}\"", local_type_name(name)),
        TypeRef::F32 | TypeRef::F64 => "float".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "str".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "bytes".into(),
        TypeRef::Enum(name) => format!("\"{}\"", local_type_name(name)),
        TypeRef::Struct(name) => format!("\"{}\"", local_type_name(name)),
        TypeRef::Optional(inner) => format!("Optional[{}]", py_type_hint(inner)),
        TypeRef::List(inner) => format!("List[{}]", py_type_hint(inner)),
        TypeRef::Map(k, v) => format!("Dict[{}, {}]", py_type_hint(k), py_type_hint(v)),
        TypeRef::Iterator(inner) => format!("Iterator[{}]", py_type_hint(inner)),
    }
}

/// Maps a shared ABI [`CType`] onto its `ctypes` spelling. The structural
/// lowering (which slots exist, in what order) comes from
/// [`weaveffi_core::abi`]; this is the Python-specific vocabulary applied to
/// each slot. Opaque handles and structs collapse to `c_void_p`; `char*`
/// becomes the `c_char_p` convenience type.
fn py_ctype(ty: &CType) -> String {
    match ty {
        CType::Int8 => "ctypes.c_int8".into(),
        CType::Int16 => "ctypes.c_int16".into(),
        CType::Int32 => "ctypes.c_int32".into(),
        CType::Uint16 => "ctypes.c_uint16".into(),
        CType::Uint32 => "ctypes.c_uint32".into(),
        CType::Int64 => "ctypes.c_int64".into(),
        CType::Uint64 => "ctypes.c_uint64".into(),
        CType::Float => "ctypes.c_float".into(),
        CType::Double => "ctypes.c_double".into(),
        CType::Bool => "ctypes.c_int32".into(),
        CType::Size => "ctypes.c_size_t".into(),
        CType::Handle => "ctypes.c_uint64".into(),
        CType::Char => "ctypes.c_char".into(),
        CType::Uint8 => "ctypes.c_uint8".into(),
        CType::Void => "None".into(),
        CType::Enum { .. } => "ctypes.c_int32".into(),
        CType::CancelToken | CType::Error | CType::StructTag { .. } | CType::Named(_) => {
            "ctypes.c_void_p".into()
        }
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Char => "ctypes.c_char_p".into(),
            CType::StructTag { .. } | CType::CancelToken | CType::Void | CType::Named(_) => {
                "ctypes.c_void_p".into()
            }
            other => format!("ctypes.POINTER({})", py_ctype(other)),
        },
    }
}

fn py_param_argtypes(ty: &TypeRef) -> Vec<String> {
    abi::lower_param("_", ty, "", false)
        .iter()
        .map(|p| py_ctype(&p.ty))
        .collect()
}

/// Returns `(restype, out_param_argtypes)` for a return type.
fn py_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    // Map returns marshal via `byref` out-params, which ctypes models with an
    // extra `POINTER` level beyond the shared C ABI shape. This convention is
    // Python-specific, so it stays local rather than in the shared model.
    if let Some((k, v)) = get_map_kv(ty) {
        return (
            "None".into(),
            vec![
                format!("ctypes.POINTER(ctypes.POINTER({}))", py_ctypes_scalar(k)),
                format!("ctypes.POINTER(ctypes.POINTER({}))", py_ctypes_scalar(v)),
                "ctypes.POINTER(ctypes.c_size_t)".into(),
            ],
        );
    }
    // Iterator constructors return the opaque iterator handle; the `_next`
    // signature is emitted separately by the iterator code path.
    if matches!(ty, TypeRef::Iterator(_)) {
        return ("ctypes.c_void_p".into(), vec![]);
    }
    let r = abi::lower_return(ty, "");
    let out = r.out_params.iter().map(|p| py_ctype(&p.ty)).collect();
    (py_ctype(&r.ret), out)
}

fn get_map_kv(ty: &TypeRef) -> Option<(&TypeRef, &TypeRef)> {
    match ty {
        TypeRef::Map(k, v) => Some((k, v)),
        TypeRef::Optional(inner) => get_map_kv(inner),
        _ => None,
    }
}

/// `(param_name, ctypes_type)` pairs for async C callback parameters after `(context, err)`.
fn py_async_cb_trailing_fields(ret: &Option<TypeRef>) -> Vec<(String, String)> {
    match ret {
        None => vec![],
        // Optional peeling stays local so `Optional<bytes>`/`<list>`/`<map>`
        // still surface their trailing `result_len`, matching the inner type.
        Some(TypeRef::Optional(inner)) if is_c_pointer_type(inner) => {
            py_async_cb_trailing_fields(&Some((**inner).clone()))
        }
        Some(ty) => abi::callback_result_params(ty, "")
            .into_iter()
            .map(|p| (p.name, py_ctype(&p.ty)))
            .collect(),
    }
}

fn append_async_success_handler(out: &mut String, ret: &Option<TypeRef>, ind: &str) {
    match ret {
        None => {
            out.push_str(&format!("{ind}_state[\"val\"] = None\n"));
        }
        Some(
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Handle,
        ) => {
            out.push_str(&format!("{ind}_state[\"val\"] = result\n"));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!("{ind}_state[\"val\"] = bool(result)\n"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(&format!(
                "{ind}_s = _bytes_to_string(result) or \"\" if result else \"\"\n"
            ));
            out.push_str(&format!("{ind}if result:\n"));
            out.push_str(&format!("{ind}    _lib.weaveffi_free_string(result)\n"));
            out.push_str(&format!("{ind}_state[\"val\"] = _s\n"));
        }
        Some(TypeRef::Enum(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}_state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::Struct(name)) | Some(TypeRef::TypedHandle(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if result is None:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"err\"] = WeaveFFIError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = b\"\"\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _n = int(result_len)\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = bytes(result[:_n])\n"));
            out.push_str(&format!(
                "{ind}    _lib.weaveffi_free_bytes(result, ctypes.c_size_t(_n))\n"
            ));
        }
        Some(TypeRef::List(inner)) => {
            let elem = py_read_element("result[_i]", inner);
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = []\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _rl = int(result_len)\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = [{elem} for _i in range(_rl)]\n"
            ));
        }
        Some(TypeRef::Map(k, v)) => {
            let kread = py_read_element("result_keys[_i]", k);
            let vread = py_read_element("result_values[_i]", v);
            out.push_str(&format!("{ind}if not result_keys or not result_values:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {{}}\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _ml = int(result_len)\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = {{{kread}: {vread} for _i in range(_ml)}}\n"
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            if is_c_pointer_type(inner) {
                match inner.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _s = _bytes_to_string(result)\n"));
                        out.push_str(&format!("{ind}    _lib.weaveffi_free_string(result)\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = _s\n"));
                    }
                    TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                        let name = local_type_name(name);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
                    }
                    TypeRef::Bytes | TypeRef::BorrowedBytes => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _n = int(result_len)\n"));
                        out.push_str(&format!("{ind}    _b = bytes(result[:_n])\n"));
                        out.push_str(&format!(
                            "{ind}    _lib.weaveffi_free_bytes(result, ctypes.c_size_t(_n))\n"
                        ));
                        out.push_str(&format!("{ind}    _state[\"val\"] = _b\n"));
                    }
                    TypeRef::List(elem) => {
                        let read = py_read_element("result[_i]", elem);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _rl = int(result_len)\n"));
                        out.push_str(&format!(
                            "{ind}    _state[\"val\"] = [{read} for _i in range(_rl)]\n"
                        ));
                    }
                    TypeRef::Map(k, v) => {
                        let kread = py_read_element("result_keys[_i]", k);
                        let vread = py_read_element("result_values[_i]", v);
                        out.push_str(&format!("{ind}if not result_keys or not result_values:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _ml = int(result_len)\n"));
                        out.push_str(&format!(
                            "{ind}    _state[\"val\"] = {{{kread}: {vread} for _i in range(_ml)}}\n"
                        ));
                    }
                    _ => append_async_success_handler(out, &Some(*inner.clone()), ind),
                }
            } else {
                match inner.as_ref() {
                    TypeRef::Bool => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = bool(result[0])\n"));
                    }
                    TypeRef::Enum(name) => {
                        let name = local_type_name(name);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result[0])\n"));
                    }
                    _ => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = result[0]\n"));
                    }
                }
            }
        }
        // Validation rejects `async` + `iter<T>` (AsyncIteratorReturn), so an
        // iterator can never reach an async completion handler.
        Some(TypeRef::Iterator(_)) => unreachable!("async iterator returns are rejected upstream"),
    }
}

fn render_async_ffi_call_body(out: &mut String, f: &FnBinding) {
    let c_async = format!("{}_async", f.c_base);
    let ind = "    ";

    out.push_str(&format!("{ind}_fn = _lib.{c_async}\n"));
    out.push_str(&format!("{ind}_ev = threading.Event()\n"));
    out.push_str(&format!("{ind}_state = {{\"err\": None, \"val\": None}}\n"));

    let trailing = py_async_cb_trailing_fields(&f.ret);
    let mut cb_param_list: Vec<String> = vec!["context".into(), "err".into()];
    cb_param_list.extend(trailing.iter().map(|(n, _)| n.clone()));
    let cb_params_joined = cb_param_list.join(", ");

    out.push_str(&format!("{ind}def _cb_impl({cb_params_joined}):\n"));
    out.push_str(&format!("{ind}    try:\n"));
    out.push_str(&format!(
        "{ind}        if err and err.contents.code != 0:\n"
    ));
    out.push_str(&format!("{ind}            _code = err.contents.code\n"));
    out.push_str(&format!(
        "{ind}            _msg = err.contents.message.decode(\"utf-8\") if err.contents.message else \"\"\n"
    ));
    out.push_str(&format!(
        "{ind}            _lib.weaveffi_error_clear(ctypes.byref(err.contents))\n"
    ));
    out.push_str(&format!(
        "{ind}            _state[\"err\"] = WeaveFFIError(_code, _msg)\n"
    ));
    out.push_str(&format!("{ind}        else:\n"));
    append_async_success_handler(out, &f.ret, "                ");
    out.push_str(&format!("{ind}    finally:\n"));
    out.push_str(&format!("{ind}        _ev.set()\n"));

    let mut cf_parts: Vec<String> = vec![
        "ctypes.c_void_p".into(),
        "ctypes.POINTER(_WeaveFFIErrorStruct)".into(),
    ];
    cf_parts.extend(trailing.iter().map(|(_, t)| t.clone()));
    out.push_str(&format!(
        "{ind}_cb_type = ctypes.CFUNCTYPE(None, {})\n",
        cf_parts.join(", ")
    ));
    out.push_str(&format!("{ind}_cb = _cb_type(_cb_impl)\n"));

    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(py_param_argtypes(&p.ty));
    }
    if f.cancellable {
        argtypes.push("ctypes.c_void_p".into());
    }
    argtypes.push("_cb_type".into());
    argtypes.push("ctypes.c_void_p".into());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = None\n"));

    for p in &f.params {
        for line in py_param_conversion(&p.name, &p.ty, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        call_args.extend(py_param_call_args(&p.name, &p.ty));
    }
    if f.cancellable {
        call_args.push("None".into());
    }
    call_args.push("_cb".into());
    call_args.push("None".into());

    out.push_str(&format!("{ind}_fn({})\n", call_args.join(", ")));
    out.push_str(&format!("{ind}_ev.wait()\n"));
    out.push_str(&format!("{ind}if _state[\"err\"] is not None:\n"));
    out.push_str(&format!("{ind}    raise _state[\"err\"]\n"));
    if f.ret.is_some() {
        out.push_str(&format!("{ind}return _state[\"val\"]\n"));
    }
}

// ── Rendering ──

/// Render the `weaveffi.py` module source. Thin wrapper over the shared
/// [`LanguageBackend::emit_members`] walk (via
/// [`PythonGenerator::render_py_source`]); retained for direct use in tests.
#[cfg(test)]
fn render_python_module(
    api: &Api,
    strip_module_prefix: bool,
    prefix: &str,
    input_basename: &str,
) -> String {
    let model = BindingModel::build(api, prefix);
    PythonGenerator.render_py_source(&model, strip_module_prefix, input_basename)
}

/// Emits a Python `# ...` line comment at `indent`. Used above C ABI binding
/// declarations (`attach_function`-style binds) where docstrings can't live.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Hash);
}

/// Emits a Python triple-quoted `"""..."""` docstring as the first statement
/// of a class or function body, at the given `indent`.
fn emit_docstring(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("\"\"\"\n");
        for line in doc.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(indent);
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(indent);
        out.push_str("\"\"\"\n");
    } else {
        out.push_str(indent);
        out.push_str("\"\"\"");
        out.push_str(doc);
        out.push_str("\"\"\"\n");
    }
}

/// Emits a NumPy/Google-style docstring with a `Parameters` section listing
/// each parameter that has a `doc:` value. Skips entirely when there is
/// nothing to document.
fn emit_fn_docstring(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&ParamBinding> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    out.push_str(indent);
    out.push_str("\"\"\"");
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push('\n');
            for line in d.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(indent);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(d);
            out.push('\n');
        }
    } else {
        out.push('\n');
    }
    if !documented_params.is_empty() {
        out.push('\n');
        out.push_str(indent);
        out.push_str("Parameters\n");
        out.push_str(indent);
        out.push_str("----------\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("{} : {}\n", p.name, first));
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str("    ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str(indent);
    out.push_str("\"\"\"\n");
}

fn render_preamble(out: &mut String) {
    out.push_str(
        r#""""WeaveFFI Python ctypes bindings (auto-generated)"""
import contextlib
import ctypes
import os
import platform
from enum import IntEnum
from typing import Callable, Dict, Iterator, List, Optional


class WeaveFFIError(Exception):
    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"({code}) {message}")


class _WeaveFFIErrorStruct(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
    ]


def _load_library() -> ctypes.CDLL:
    # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
    # specific build artifact regardless of its file name or location.
    override = os.environ.get("WEAVEFFI_LIBRARY")
    if override:
        return ctypes.CDLL(override)
    system = platform.system()
    if system == "Darwin":
        name = "libweaveffi.dylib"
    elif system == "Windows":
        name = "weaveffi.dll"
    else:
        name = "libweaveffi.so"
    return ctypes.CDLL(name)


_lib = _load_library()
_lib.weaveffi_error_clear.argtypes = [ctypes.POINTER(_WeaveFFIErrorStruct)]
_lib.weaveffi_error_clear.restype = None
_lib.weaveffi_free_string.argtypes = [ctypes.c_char_p]
_lib.weaveffi_free_string.restype = None
_lib.weaveffi_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
_lib.weaveffi_free_bytes.restype = None


def _check_error(err: _WeaveFFIErrorStruct) -> None:
    if err.code != 0:
        code = err.code
        message = err.message.decode("utf-8") if err.message else ""
        _lib.weaveffi_error_clear(ctypes.byref(err))
        raise WeaveFFIError(code, message)


class _PointerGuard(contextlib.AbstractContextManager):
    def __init__(self, ptr, free_fn) -> None:
        self.ptr = ptr
        self._free_fn = free_fn

    def __exit__(self, *exc) -> bool:
        if self.ptr is not None:
            self._free_fn(self.ptr)
            self.ptr = None
        return False


def _string_to_bytes(s: Optional[str]) -> Optional[bytes]:
    if s is None:
        return None
    return s.encode("utf-8")


def _bytes_to_string(ptr) -> Optional[str]:
    if ptr is None:
        return None
    return ptr.decode("utf-8")
"#,
    );
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // Rich (algebraic) enums cross the ABI as opaque objects, so they are
    // emitted as wrapper classes (like structs), not plain `IntEnum`s.
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    out.push_str(&format!("\n\nclass {}(IntEnum):\n", e.name));
    emit_docstring(out, &e.doc, "    ");
    for v in &e.variants {
        if let Some(d) = &v.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    out.push_str(&format!("    # {}\n", line));
                }
            }
        }
        out.push_str(&format!("    {} = {}\n", v.name, v.value));
    }
}

/// Render a rich (algebraic) enum as an opaque-object wrapper class, mirroring
/// the Python struct wrapper: it owns the C handle and frees it once in
/// `__del__` (matching [`render_struct`]), exposes a nested `Tag` `IntEnum`
/// plus a `tag` property reading the active discriminant, one `@classmethod`
/// factory per variant (`Shape.circle(radius)`), and per-variant field
/// accessors namespaced by variant (`circle_radius`). The opaque-object surface
/// (tag/destroy symbols, per-variant constructors and field getters) is
/// precomputed in the binding model exactly like a struct's.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let rich = e
        .rich
        .as_ref()
        .expect("render_rich_enum requires a rich (algebraic) enum");
    let name = &e.name;
    let destroy = &rich.destroy_symbol;
    let tag_symbol = &rich.tag_symbol;

    out.push_str(&format!("\n\nclass {}:\n", name));
    emit_docstring(out, &e.doc, "    ");

    // Nested discriminant enum (`Shape.Tag.Circle == 1`, …).
    out.push_str("\n    class Tag(IntEnum):\n");
    for v in &e.variants {
        if let Some(d) = &v.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    out.push_str(&format!("        # {}\n", line));
                }
            }
        }
        out.push_str(&format!("        {} = {}\n", v.name, v.value));
    }

    // Ownership: keep the raw pointer and free it exactly once (no double-free).
    out.push_str("\n    def __init__(self, _ptr: int) -> None:");
    out.push_str("\n        self._ptr = _ptr");

    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{destroy}.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!("\n            _lib.{destroy}.restype = None"));
    out.push_str(&format!("\n            _lib.{destroy}(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    // tag: read the active variant's discriminant (an `int`, comparable to the
    // nested `Tag` members).
    out.push_str("\n\n    @property\n    def tag(self) -> int:");
    out.push_str(&format!("\n        _fn = _lib.{tag_symbol}"));
    out.push_str("\n        _fn.argtypes = [ctypes.c_void_p]");
    out.push_str("\n        _fn.restype = ctypes.c_int32");
    out.push_str("\n        return _fn(self._ptr)");

    // One factory classmethod per variant (`Shape.circle(2.5)`).
    for v in &rich.variants {
        render_rich_variant_factory(out, name, v);
    }

    // Per-variant field accessors, namespaced by variant to avoid collisions.
    // Reuse the struct getter renderer (identical marshalling: string decode,
    // bytes/list length out-params, wrapper construction, …) by projecting the
    // namespaced Python name onto the field's precomputed getter symbol.
    for v in &rich.variants {
        let variant_snake = v.name.to_snake_case();
        for f in &v.fields {
            let mut namespaced = f.clone();
            namespaced.name = format!("{variant_snake}_{}", f.name);
            render_getter(out, &namespaced);
        }
    }
    out.push('\n');
}

/// One variant constructor as a `@classmethod` factory. Mirrors the struct
/// builder's `build()` marshalling: each variant field lowers to the same ABI
/// argument slots, the call threads an `out_err` and is checked with
/// `_check_error`, and the returned handle is wrapped (`return cls(_result)`).
fn render_rich_variant_factory(out: &mut String, enum_name: &str, v: &RichVariantBinding) {
    let factory = v.name.to_snake_case();
    let ind = "        ";

    let params_sig: Vec<String> = v
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, py_type_hint(&f.ty)))
        .collect();
    let sig = if params_sig.is_empty() {
        "cls".to_string()
    } else {
        format!("cls, {}", params_sig.join(", "))
    };
    out.push_str(&format!(
        "\n\n    @classmethod\n    def {factory}({sig}) -> \"{enum_name}\":\n"
    ));
    emit_docstring(out, &v.doc, ind);

    out.push_str(&format!("{ind}_fn = _lib.{}\n", v.create.symbol));
    let mut argtypes: Vec<String> = Vec::new();
    for f in &v.fields {
        argtypes.extend(py_param_argtypes(&f.ty));
    }
    argtypes.push("ctypes.POINTER(_WeaveFFIErrorStruct)".into());
    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = ctypes.c_void_p\n"));

    for f in &v.fields {
        for line in py_param_conversion(&f.name, &f.ty, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    out.push_str(&format!("{ind}_err = _WeaveFFIErrorStruct()\n"));
    let mut call_args: Vec<String> = Vec::new();
    for f in &v.fields {
        call_args.extend(py_param_call_args(&f.name, &f.ty));
    }
    call_args.push("ctypes.byref(_err)".into());
    out.push_str(&format!("{ind}_result = _fn({})\n", call_args.join(", ")));
    out.push_str(&format!("{ind}_check_error(_err)\n"));
    out.push_str(&format!("{ind}if _result is None:\n"));
    out.push_str(&format!(
        "{ind}    raise WeaveFFIError(-1, \"null pointer\")\n"
    ));
    out.push_str(&format!("{ind}return cls(_result)\n"));
}

fn render_struct(out: &mut String, s: &StructBinding) {
    let destroy = &s.destroy_symbol;

    out.push_str(&format!("\n\nclass {}:\n", s.name));
    emit_docstring(out, &s.doc, "    ");

    out.push_str("\n    def __init__(self, _ptr: int) -> None:");
    out.push_str("\n        self._ptr = _ptr");

    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{destroy}.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!("\n            _lib.{destroy}.restype = None"));
    out.push_str(&format!("\n            _lib.{destroy}(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    for field in &s.fields {
        render_getter(out, field);
    }
    out.push('\n');
}

fn render_builder(out: &mut String, s: &StructBinding) {
    let builder_name = format!("{}Builder", s.name);
    out.push_str(&format!("\n\nclass {}:\n", builder_name));
    emit_docstring(out, &s.doc, "    ");
    out.push_str("    def __init__(self) -> None:");
    // Zero-value defaults (the same contract as the other backends): scalars
    // start at 0/False/""/b"", collections empty, optionals absent. Unset
    // fields therefore lower to valid C arguments instead of raising.
    for field in &s.fields {
        let (default, hint) = py_field_default(&field.ty);
        out.push_str(&format!(
            "\n        self._{}: {} = {}",
            field.name, hint, default
        ));
    }
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        out.push_str(&format!(
            "\n\n    def with_{}(self, value: {}) -> \"{}\":",
            field.name, py_ty, builder_name
        ));
        if let Some(d) = &field.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                if trimmed.contains('\n') {
                    out.push_str("\n        \"\"\"\n");
                    for line in trimmed.lines() {
                        if line.is_empty() {
                            out.push('\n');
                        } else {
                            out.push_str("        ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                    out.push_str("        \"\"\"");
                } else {
                    out.push_str(&format!("\n        \"\"\"{}\"\"\"", trimmed));
                }
            }
        }
        out.push_str(&format!("\n        self._{} = value", field.name));
        out.push_str("\n        return self");
    }
    let ret_ty = py_type_hint(&TypeRef::Struct(s.name.clone()));
    out.push_str(&format!("\n\n    def build(self) -> {}:", ret_ty));
    // Marshal every field into the struct's C `create` call with the same
    // lowering used for function parameters, then wrap the returned handle.
    let ind = "        ";
    for field in &s.fields {
        out.push_str(&format!("\n{ind}{} = self._{}", field.name, field.name));
    }
    out.push_str(&format!("\n{ind}_fn = _lib.{}", s.create.symbol));
    let mut argtypes: Vec<String> = Vec::new();
    for field in &s.fields {
        argtypes.extend(py_param_argtypes(&field.ty));
    }
    argtypes.push("ctypes.POINTER(_WeaveFFIErrorStruct)".into());
    out.push_str(&format!("\n{ind}_fn.argtypes = [{}]", argtypes.join(", ")));
    out.push_str(&format!("\n{ind}_fn.restype = ctypes.c_void_p"));
    for field in &s.fields {
        for line in py_param_conversion(&field.name, &field.ty, ind) {
            out.push('\n');
            out.push_str(&line);
        }
    }
    out.push_str(&format!("\n{ind}_err = _WeaveFFIErrorStruct()"));
    let mut call_args: Vec<String> = Vec::new();
    for field in &s.fields {
        call_args.extend(py_param_call_args(&field.name, &field.ty));
    }
    call_args.push("ctypes.byref(_err)".into());
    out.push_str(&format!("\n{ind}_result = _fn({})", call_args.join(", ")));
    out.push_str(&format!("\n{ind}_check_error(_err)"));
    out.push_str(&format!("\n{ind}if _result is None:"));
    out.push_str(&format!(
        "\n{ind}    raise WeaveFFIError(-1, \"null pointer\")"
    ));
    out.push_str(&format!("\n{ind}return {}(_result)", s.name));
    out.push('\n');
}

/// The zero-value default (and matching type hint) for one builder slot.
fn py_field_default(ty: &TypeRef) -> (String, String) {
    let hint = py_type_hint(ty);
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::Handle => ("0".into(), hint),
        TypeRef::F32 | TypeRef::F64 => ("0.0".into(), hint),
        TypeRef::Bool => ("False".into(), hint),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("\"\"".into(), hint),
        TypeRef::Bytes | TypeRef::BorrowedBytes => ("b\"\"".into(), hint),
        TypeRef::List(_) => ("[]".into(), hint),
        TypeRef::Map(_, _) => ("{}".into(), hint),
        TypeRef::Optional(_) => ("None".into(), hint),
        // No synthesizable zero value; the with_ setter is the only path.
        _ => ("None".into(), format!("Optional[{hint}]")),
    }
}

// ── Callbacks & listeners ──

/// The module-level `ctypes.CFUNCTYPE` alias for one callback type. Listener
/// registration binds against this; the C side sees the matching
/// `typedef void (*{c_fn_type})(…, void* context)`.
fn render_callback_type(out: &mut String, c: &CallbackBinding) {
    let mut parts: Vec<String> = vec!["None".into()];
    parts.extend(c.abi_params.iter().map(|p| py_ctype(&p.ty)));
    out.push_str("\n\n");
    emit_doc(out, &c.doc, "");
    out.push_str(&format!(
        "# Callback type {}: {}\n",
        c.name,
        py_callable_hint(&c.params)
    ));
    out.push_str(&format!(
        "_CFUNC_{} = ctypes.CFUNCTYPE({})\n",
        c.c_fn_type,
        parts.join(", ")
    ));
}

/// `Callable[[<param hints>], None]` for a callback's idiomatic signature.
fn py_callable_hint(params: &[ParamBinding]) -> String {
    let hints: Vec<String> = params.iter().map(|p| py_type_hint(&p.ty)).collect();
    format!("Callable[[{}], None]", hints.join(", "))
}

/// The Python expression converting one trampoline parameter's C slots into
/// the idiomatic value passed to the user callback. `n` is the IR parameter
/// name (slot names derive from it, mirroring [`abi::lower_param`]).
fn py_cb_param_expr(n: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => n.into(),
        TypeRef::Bool => format!("bool({n})"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({n})"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("bytes({n}_ptr[:{n}_len]) if {n}_ptr else b\"\"")
        }
        TypeRef::Enum(name) => format!("{}({n})", local_type_name(name)),
        // Borrowed by contract: the producer owns callback arguments for the
        // duration of the call, so opaque pointers pass through raw rather
        // than being wrapped in an owning class whose __del__ would free them.
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n.into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({n})"),
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                format!("bytes({n}_ptr[:{n}_len]) if {n}_ptr else None")
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n.into(),
            TypeRef::List(elem) => {
                let read = py_read_element(&format!("{n}[_i]"), elem);
                format!("[{read} for _i in range({n}_len)] if {n} else None")
            }
            TypeRef::Map(k, v) => {
                let kread = py_read_element(&format!("{n}_keys[_i]"), k);
                let vread = py_read_element(&format!("{n}_values[_i]"), v);
                format!("{{{kread}: {vread} for _i in range({n}_len)}} if {n}_keys else None")
            }
            TypeRef::Bool => format!("bool({n}[0]) if {n} else None"),
            TypeRef::Enum(name) => format!("{}({n}[0]) if {n} else None", local_type_name(name)),
            _ => format!("{n}[0] if {n} else None"),
        },
        TypeRef::List(inner) => {
            let read = py_read_element(&format!("{n}[_i]"), inner);
            format!("[{read} for _i in range({n}_len)] if {n} else []")
        }
        TypeRef::Map(k, v) => {
            let kread = py_read_element(&format!("{n}_keys[_i]"), k);
            let vread = py_read_element(&format!("{n}_values[_i]"), v);
            format!("{{{kread}: {vread} for _i in range({n}_len)}} if {n}_keys else {{}}")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// Register/unregister wrapper pair for one listener. The trampoline converts
/// each C slot to its idiomatic value, and the `ctypes` function object is
/// pinned in `_listener_refs` until `unregister` so the producer never calls
/// a collected trampoline.
fn render_listener(
    out: &mut String,
    module: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = module.callbacks.iter().find(|c| c.name == l.event_callback) else {
        // Validation guarantees the referenced callback exists in-module.
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = wrapper_name(
        &module.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    let unregister_name = wrapper_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );
    let cfunc = format!("_CFUNC_{}", cb.c_fn_type);
    let ind = "    ";

    // register_{listener}(callback) -> int
    out.push_str(&format!(
        "\n\ndef {register_name}(callback: {}) -> int:\n",
        py_callable_hint(&cb.params)
    ));
    let reg_doc = match &l.doc {
        Some(d) => format!(
            "{}\n\nReturns a subscription id for {unregister_name}().",
            d.trim()
        ),
        None => format!(
            "Register a {} listener. Returns a subscription id for {unregister_name}().",
            cb.name
        ),
    };
    emit_docstring(out, &Some(reg_doc), ind);

    let tramp_params: Vec<String> = cb
        .params
        .iter()
        .flat_map(|p| p.abi.iter().map(|slot| slot.name.clone()))
        .chain(std::iter::once("_context".to_string()))
        .collect();
    let call_args: Vec<String> = cb
        .params
        .iter()
        .map(|p| py_cb_param_expr(&p.name, &p.ty))
        .collect();
    out.push_str(&format!(
        "{ind}def _trampoline({}):\n",
        tramp_params.join(", ")
    ));
    out.push_str(&format!("{ind}    callback({})\n", call_args.join(", ")));
    out.push_str(&format!("{ind}_cfunc = {cfunc}(_trampoline)\n"));
    out.push_str(&format!("{ind}_fn = _lib.{}\n", l.register_symbol));
    out.push_str(&format!("{ind}_fn.argtypes = [{cfunc}, ctypes.c_void_p]\n"));
    out.push_str(&format!("{ind}_fn.restype = ctypes.c_uint64\n"));
    out.push_str(&format!("{ind}_listener_id = int(_fn(_cfunc, None))\n"));
    out.push_str(&format!("{ind}_listener_refs[_listener_id] = _cfunc\n"));
    out.push_str(&format!("{ind}return _listener_id\n"));

    // unregister_{listener}(listener_id) -> None
    out.push_str(&format!(
        "\n\ndef {unregister_name}(listener_id: int) -> None:\n"
    ));
    emit_docstring(
        out,
        &Some(format!(
            "Unregister a listener previously registered with {register_name}()."
        )),
        ind,
    );
    out.push_str(&format!("{ind}_fn = _lib.{}\n", l.unregister_symbol));
    out.push_str(&format!("{ind}_fn.argtypes = [ctypes.c_uint64]\n"));
    out.push_str(&format!("{ind}_fn.restype = None\n"));
    out.push_str(&format!("{ind}_fn(ctypes.c_uint64(listener_id))\n"));
    out.push_str(&format!("{ind}_listener_refs.pop(listener_id, None)\n"));
}

fn render_getter(out: &mut String, field: &FieldBinding) {
    let getter = &field.getter_symbol;
    let py_ty = py_type_hint(&field.ty);
    let ind = "        ";

    out.push_str(&format!(
        "\n\n    @property\n    def {}(self) -> {}:\n",
        field.name, py_ty
    ));
    emit_docstring(out, &field.doc, ind);
    out.push_str(&format!("{ind}_fn = _lib.{getter}\n"));

    let (restype, out_argtypes) = py_return_info(&field.ty);
    let mut argtypes = vec!["ctypes.c_void_p".to_string()];
    argtypes.extend(out_argtypes.iter().cloned());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = {restype}\n"));

    if out_argtypes.is_empty() {
        out.push_str(&format!("{ind}_result = _fn(self._ptr)\n"));
    } else if let Some((k, v)) = get_map_kv(&field.ty) {
        out.push_str(&format!(
            "{ind}_out_keys = ctypes.POINTER({})()\n",
            py_ctypes_scalar(k)
        ));
        out.push_str(&format!(
            "{ind}_out_values = ctypes.POINTER({})()\n",
            py_ctypes_scalar(v)
        ));
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        out.push_str(&format!("{ind}_fn(self._ptr, ctypes.byref(_out_keys), ctypes.byref(_out_values), ctypes.byref(_out_len))\n"));
    } else {
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        out.push_str(&format!(
            "{ind}_result = _fn(self._ptr, ctypes.byref(_out_len))\n"
        ));
    }

    render_return_value(out, &field.ty, ind);
}

fn render_function(out: &mut String, module_name: &str, f: &FnBinding, strip_module_prefix: bool) {
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, py_type_hint(&p.ty)))
        .collect();
    let ret_hint = f
        .ret
        .as_ref()
        .map(py_type_hint)
        .unwrap_or_else(|| "None".to_string());

    let def_name = if f.is_async {
        format!("_{func_name}_sync")
    } else {
        func_name.clone()
    };

    if let (Some(TypeRef::Iterator(inner)), CallShape::Iterator(it)) = (&f.ret, &f.shape) {
        render_iterator_class(out, &it.iter_tag, &f.name, inner);
    }

    out.push_str(&format!(
        "\n\ndef {}({}) -> {}:\n",
        def_name,
        params_sig.join(", "),
        ret_hint
    ));

    let ind = "    ";

    emit_fn_docstring(out, &f.doc, &f.params, ind);

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "{ind}import warnings\n{ind}warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)\n",
            msg.replace('"', "\\\"")
        ));
    }

    if f.is_async {
        render_async_ffi_call_body(out, f);
    } else {
        out.push_str(&format!("{ind}_fn = _lib.{}\n", f.c_base));

        let mut argtypes: Vec<String> = Vec::new();
        for p in &f.params {
            argtypes.extend(py_param_argtypes(&p.ty));
        }
        let mut out_ret_argtypes = Vec::new();
        let restype;
        if let Some(ret_ty) = &f.ret {
            let (rt, oat) = py_return_info(ret_ty);
            argtypes.extend(oat.iter().cloned());
            restype = rt;
            out_ret_argtypes = oat;
        } else {
            restype = "None".to_string();
        }
        argtypes.push("ctypes.POINTER(_WeaveFFIErrorStruct)".into());

        out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
        out.push_str(&format!("{ind}_fn.restype = {restype}\n"));

        for p in &f.params {
            for line in py_param_conversion(&p.name, &p.ty, ind) {
                out.push_str(&line);
                out.push('\n');
            }
        }

        out.push_str(&format!("{ind}_err = _WeaveFFIErrorStruct()\n"));

        let is_map_ret = f.ret.as_ref().and_then(get_map_kv).is_some();
        let has_out_len = !out_ret_argtypes.is_empty() && !is_map_ret;

        if let Some((k, v)) = f.ret.as_ref().and_then(get_map_kv) {
            out.push_str(&format!(
                "{ind}_out_keys = ctypes.POINTER({})()\n",
                py_ctypes_scalar(k)
            ));
            out.push_str(&format!(
                "{ind}_out_values = ctypes.POINTER({})()\n",
                py_ctypes_scalar(v)
            ));
            out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        } else if has_out_len {
            out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        }

        let mut call_args: Vec<String> = Vec::new();
        for p in &f.params {
            call_args.extend(py_param_call_args(&p.name, &p.ty));
        }
        if is_map_ret {
            call_args.push("ctypes.byref(_out_keys)".into());
            call_args.push("ctypes.byref(_out_values)".into());
            call_args.push("ctypes.byref(_out_len)".into());
        } else if has_out_len {
            call_args.push("ctypes.byref(_out_len)".into());
        }
        call_args.push("ctypes.byref(_err)".into());

        let call_expr = format!("_fn({})", call_args.join(", "));
        if f.ret.is_some() && !is_map_ret {
            out.push_str(&format!("{ind}_result = {call_expr}\n"));
        } else {
            out.push_str(&format!("{ind}{call_expr}\n"));
        }

        out.push_str(&format!("{ind}_check_error(_err)\n"));

        if let Some(ret_ty) = &f.ret {
            if let (TypeRef::Iterator(inner), CallShape::Iterator(it)) = (ret_ty, &f.shape) {
                render_iterator_return(out, &it.iter_tag, inner, ind);
            } else {
                render_return_value(out, ret_ty, ind);
            }
        }
    }

    if f.is_async {
        let params_joined = params_sig.join(", ");
        out.push_str(&format!(
            "\n\nasync def {}({}) -> {}:\n",
            func_name, params_joined, ret_hint
        ));
        emit_fn_docstring(out, &f.doc, &f.params, ind);
        out.push_str("    _loop = asyncio.get_event_loop()\n");
        let arg_names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        let executor_args = if arg_names.is_empty() {
            def_name
        } else {
            format!("{def_name}, {}", arg_names.join(", "))
        };
        if f.ret.is_some() {
            out.push_str(&format!(
                "    return await _loop.run_in_executor(None, {executor_args})\n"
            ));
        } else {
            out.push_str(&format!(
                "    await _loop.run_in_executor(None, {executor_args})\n"
            ));
        }
    }
}

// ── Param helpers ──

fn py_list_convert_expr(name: &str, elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[_string_to_bytes(v) for v in {name}]")
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("*[v._ptr for v in {name}]"),
        TypeRef::Enum(_) => format!("*[v.value for v in {name}]"),
        TypeRef::Bool => format!("*[1 if v else 0 for v in {name}]"),
        _ => format!("*{name}"),
    }
}

fn py_map_elem_convert(list_name: &str, ty: &TypeRef, var: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[_string_to_bytes({var}) for {var} in {list_name}]")
        }
        TypeRef::Enum(_) => format!("*[{var}.value for {var} in {list_name}]"),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            format!("*[{var}._ptr for {var} in {list_name}]")
        }
        TypeRef::Bool => format!("*[1 if {var} else 0 for {var} in {list_name}]"),
        _ => format!("*{list_name}"),
    }
}

fn py_param_conversion(name: &str, ty: &TypeRef, ind: &str) -> Vec<String> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let s = py_ctypes_scalar(&TypeRef::Bytes);
            vec![format!("{ind}_{name}_arr = ({s} * len({name}))(*{name})")]
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Handle => {
                let s = py_ctypes_scalar(inner);
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref({s}({name})) if {name} is not None else None"
                )]
            }
            TypeRef::Bool => {
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref(ctypes.c_int32(1 if {name} else 0)) if {name} is not None else None"
                )]
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                vec![format!("{ind}_{name}_c = _string_to_bytes({name})")]
            }
            TypeRef::Enum(_) => {
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref(ctypes.c_int32({name}.value)) if {name} is not None else None"
                )]
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let s = py_ctypes_scalar(&TypeRef::Bytes);
                vec![
                    format!("{ind}if {name} is not None:"),
                    format!("{ind}    _{name}_arr = ({s} * len({name}))(*{name})"),
                    format!("{ind}    _{name}_len = len({name})"),
                    format!("{ind}else:"),
                    format!("{ind}    _{name}_arr = None"),
                    format!("{ind}    _{name}_len = 0"),
                ]
            }
            TypeRef::List(elem) => {
                let s = py_ctypes_scalar(elem);
                let convert = py_list_convert_expr(name, elem);
                vec![
                    format!("{ind}if {name} is not None:"),
                    format!("{ind}    _{name}_arr = ({s} * len({name}))({convert})"),
                    format!("{ind}    _{name}_len = len({name})"),
                    format!("{ind}else:"),
                    format!("{ind}    _{name}_arr = None"),
                    format!("{ind}    _{name}_len = 0"),
                ]
            }
            _ => vec![],
        },
        TypeRef::List(inner) => {
            let s = py_ctypes_scalar(inner);
            let convert = py_list_convert_expr(name, inner);
            vec![format!("{ind}_{name}_arr = ({s} * len({name}))({convert})")]
        }
        TypeRef::Map(k, v) => {
            let ks = py_ctypes_scalar(k);
            let vs = py_ctypes_scalar(v);
            let kconv = py_map_elem_convert(&format!("_{name}_keys"), k, "_k");
            let vconv = py_map_elem_convert(&format!("_{name}_vals"), v, "_v");
            vec![
                format!("{ind}_{name}_keys = list({name}.keys())"),
                format!("{ind}_{name}_vals = [{name}[_k] for _k in _{name}_keys]"),
                format!("{ind}_{name}_ka = ({ks} * len(_{name}_keys))({kconv})"),
                format!("{ind}_{name}_va = ({vs} * len(_{name}_vals))({vconv})"),
            ]
        }
        _ => vec![],
    }
}

fn py_param_call_args(name: &str, ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => {
            vec![name.to_string()]
        }
        TypeRef::Bool => vec![format!("1 if {name} else 0")],
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("_string_to_bytes({name})")],
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("_{name}_arr"), format!("len({name})")]
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}._ptr")],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("_{name}_c")],
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                vec![format!("{name}._ptr if {name} is not None else None")]
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                vec![format!("_{name}_arr"), format!("_{name}_len")]
            }
            TypeRef::Map(_, _) => vec![
                format!("_{name}_ka"),
                format!("_{name}_va"),
                format!("_{name}_len"),
            ],
            _ if !is_c_pointer_type(inner) => vec![format!("_{name}_c")],
            _ => py_param_call_args(name, inner),
        },
        TypeRef::List(_) => vec![format!("_{name}_arr"), format!("len({name})")],
        TypeRef::Map(_, _) => vec![
            format!("_{name}_ka"),
            format!("_{name}_va"),
            format!("len(_{name}_keys)"),
        ],
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

// ── Return helpers ──

fn py_read_element(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({expr})"),
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) | TypeRef::Enum(name) => {
            let name = local_type_name(name);
            format!("{name}({expr})")
        }
        TypeRef::Bool => format!("bool({expr})"),
        _ => expr.to_string(),
    }
}

fn render_return_value(out: &mut String, ty: &TypeRef, ind: &str) {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => {
            out.push_str(&format!("{ind}return _result\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}return bool(_result)\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return _bytes_to_string(_result) or \"\"\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return b\"\"\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!(
                "{ind}    raise WeaveFFIError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Enum(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Optional(inner) => render_optional_return(out, inner, ind),
        TypeRef::List(inner) => render_list_return(out, inner, ind),
        TypeRef::Map(k, v) => render_map_return(out, k, v, ind),
        TypeRef::Iterator(_) => unreachable!("iterator return handled in render_function"),
    }
}

fn render_optional_return(out: &mut String, inner: &TypeRef, ind: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return _bytes_to_string(_result)\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Enum(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return {name}(_result[0])\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return bool(_result[0])\n"));
        }
        _ if !is_c_pointer_type(inner) => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return _result[0]\n"));
        }
        _ => {
            out.push_str(&format!("{ind}return _result\n"));
        }
    }
}

fn render_list_return(out: &mut String, inner: &TypeRef, ind: &str) {
    out.push_str(&format!("{ind}if not _result:\n"));
    out.push_str(&format!("{ind}    return []\n"));
    let elem = py_read_element("_result[_i]", inner);
    out.push_str(&format!(
        "{ind}return [{elem} for _i in range(_out_len.value)]\n"
    ));
}

fn render_map_return(out: &mut String, k: &TypeRef, v: &TypeRef, ind: &str) {
    out.push_str(&format!("{ind}if not _out_keys or not _out_values:\n"));
    out.push_str(&format!("{ind}    return {{}}\n"));
    let key_read = py_read_element("_out_keys[_i]", k);
    let val_read = py_read_element("_out_values[_i]", v);
    out.push_str(&format!(
        "{ind}return {{{key_read}: {val_read} for _i in range(_out_len.value)}}\n"
    ));
}

// ── Iterator helpers ──

fn py_read_iter_item(inner: &TypeRef) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "_bytes_to_string(_out_item.value)".into(),
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) | TypeRef::Enum(name) => {
            let name = local_type_name(name);
            format!("{name}(_out_item.value)")
        }
        TypeRef::Bool => "bool(_out_item.value)".into(),
        _ => "_out_item.value".into(),
    }
}

fn render_iterator_class(out: &mut String, iter_tag: &str, func_name: &str, inner: &TypeRef) {
    let pascal = pascal_case(func_name);
    let class_name = format!("_{pascal}Iterator");
    let item_scalar = py_ctypes_scalar(inner);
    let read_expr = py_read_iter_item(inner);

    out.push_str(&format!("\n\nclass {class_name}:"));
    out.push_str("\n    def __init__(self, ptr):");
    out.push_str("\n        self._ptr = ptr");
    out.push_str("\n        self._done = False");

    out.push_str("\n\n    def __iter__(self):");
    out.push_str("\n        return self");

    out.push_str("\n\n    def __next__(self):");
    out.push_str("\n        if self._done:");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        _next_fn = _lib.{iter_tag}_next"));
    out.push_str(&format!(
        "\n        _next_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER({item_scalar}), ctypes.POINTER(_WeaveFFIErrorStruct)]"
    ));
    out.push_str("\n        _next_fn.restype = ctypes.c_int32");
    out.push_str(&format!("\n        _out_item = {item_scalar}()"));
    out.push_str("\n        _err = _WeaveFFIErrorStruct()");
    out.push_str(
        "\n        _has = _next_fn(self._ptr, ctypes.byref(_out_item), ctypes.byref(_err))",
    );
    out.push_str("\n        _check_error(_err)");
    out.push_str("\n        if not _has:");
    out.push_str("\n            self._done = True");
    out.push_str("\n            self._destroy()");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        return {read_expr}"));

    out.push_str("\n\n    def _destroy(self):");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _destroy_fn = _lib.{iter_tag}_destroy"
    ));
    out.push_str("\n            _destroy_fn.argtypes = [ctypes.c_void_p]");
    out.push_str("\n            _destroy_fn.restype = None");
    out.push_str("\n            _destroy_fn(self._ptr)");
    out.push_str("\n            self._ptr = None");

    out.push_str("\n\n    def __del__(self):");
    out.push_str("\n        self._destroy()");
    out.push('\n');
}

fn render_iterator_return(out: &mut String, iter_tag: &str, inner: &TypeRef, ind: &str) {
    let item_scalar = py_ctypes_scalar(inner);
    let read_expr = py_read_iter_item(inner);

    out.push_str(&format!("{ind}_next_fn = _lib.{iter_tag}_next\n"));
    out.push_str(&format!(
        "{ind}_next_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER({item_scalar}), ctypes.POINTER(_WeaveFFIErrorStruct)]\n"
    ));
    out.push_str(&format!("{ind}_next_fn.restype = ctypes.c_int32\n"));

    out.push_str(&format!("{ind}_destroy_fn = _lib.{iter_tag}_destroy\n"));
    out.push_str(&format!("{ind}_destroy_fn.argtypes = [ctypes.c_void_p]\n"));
    out.push_str(&format!("{ind}_destroy_fn.restype = None\n"));

    out.push_str(&format!("{ind}_items = []\n"));
    out.push_str(&format!("{ind}while True:\n"));
    out.push_str(&format!("{ind}    _out_item = {item_scalar}()\n"));
    out.push_str(&format!("{ind}    _item_err = _WeaveFFIErrorStruct()\n"));
    out.push_str(&format!(
        "{ind}    _has = _next_fn(_result, ctypes.byref(_out_item), ctypes.byref(_item_err))\n"
    ));
    out.push_str(&format!("{ind}    _check_error(_item_err)\n"));
    out.push_str(&format!("{ind}    if not _has:\n"));
    out.push_str(&format!("{ind}        break\n"));
    out.push_str(&format!("{ind}    _items.append({read_expr})\n"));

    out.push_str(&format!("{ind}_destroy_fn(_result)\n"));
    out.push_str(&format!("{ind}return _items\n"));
}

// ── Packaging ──

fn render_pyproject_toml(
    package: &ResolvedPackage,
    import_name: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pyproject.toml");
    let name = &package.name;
    let version = &package.version;
    let description = package.description_or_default();
    let mut extra = String::new();
    if let Some(license) = &package.license {
        extra.push_str(&format!("license = {{ text = \"{license}\" }}\n"));
    }
    if !package.authors.is_empty() {
        let authors = package
            .authors
            .iter()
            .map(|a| format!("{{ name = \"{a}\" }}"))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("authors = [{authors}]\n"));
    }
    if let Some(homepage) = &package.homepage {
        extra.push_str(&format!("[project.urls]\nHomepage = \"{homepage}\"\n"));
    } else if let Some(repository) = &package.repository {
        extra.push_str(&format!("[project.urls]\nRepository = \"{repository}\"\n"));
    }
    format!(
        r#"{prelude}[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"

[project]
name = "{name}"
version = "{version}"
description = "{description}"
requires-python = ">=3.8"
{extra}
[tool.setuptools]
packages = ["{import_name}"]

{trailer}"#,
    )
}

fn render_setup_py(package: &ResolvedPackage, import_name: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "setup.py");
    let name = &package.name;
    let version = &package.version;
    format!(
        r#"{prelude}from setuptools import setup

setup(
    name="{name}",
    version="{version}",
    packages=["{import_name}"],
)

{trailer}"#,
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let import_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Python)

Auto-generated Python bindings using ctypes.

## Prerequisites

- Python >= 3.8
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
pip install .
```

## Development install

```bash
pip install -e .
```

## Usage

```python
from {import_name} import *
```

{trailer}"#
    )
}

// ── Type stub (.pyi) rendering ──

fn render_pyi_module(api: &Api, strip_module_prefix: bool, input_basename: &str) -> String {
    // Type stubs contain no C symbols, so the ABI prefix is irrelevant here; the
    // model is used purely for its flattened, path-carrying module traversal.
    let model = BindingModel::build(api, "weaveffi");
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(
        "from enum import IntEnum\nfrom typing import Callable, Dict, Iterator, List, Optional\n",
    );
    for m in &model.modules {
        for e in &m.enums {
            if e.is_rich() {
                render_pyi_rich_enum(&mut out, e);
            } else {
                render_pyi_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_pyi_struct(&mut out, s);
        }
        for l in &m.listeners {
            render_pyi_listener(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            render_pyi_function(&mut out, &m.path, f, strip_module_prefix);
        }
    }
    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::Hash, "weaveffi.pyi"));
    out
}

fn render_pyi_enum(out: &mut String, e: &EnumBinding) {
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {}(IntEnum):\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!("    {}: int\n", v.name));
    }
}

/// `.pyi` stub for a rich (algebraic) enum: a class with a nested `Tag`
/// `IntEnum`, the `tag` reader, a factory classmethod per variant, and the
/// namespaced per-variant field properties, mirroring [`render_rich_enum`].
fn render_pyi_rich_enum(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {}:\n", e.name));
    out.push_str("    class Tag(IntEnum):\n");
    for v in &e.variants {
        emit_doc(out, &v.doc, "        ");
        out.push_str(&format!("        {}: int\n", v.name));
    }
    out.push_str("    @property\n    def tag(self) -> int: ...\n");
    for v in &rich.variants {
        let factory = v.name.to_snake_case();
        let params: Vec<String> = v
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, py_type_hint(&f.ty)))
            .collect();
        let sig = if params.is_empty() {
            "cls".to_string()
        } else {
            format!("cls, {}", params.join(", "))
        };
        out.push_str(&format!(
            "    @classmethod\n    def {factory}({sig}) -> \"{}\": ...\n",
            e.name
        ));
    }
    for v in &rich.variants {
        let variant_snake = v.name.to_snake_case();
        for f in &v.fields {
            out.push_str(&format!(
                "    @property\n    def {variant_snake}_{}(self) -> {}: ...\n",
                f.name,
                py_type_hint(&f.ty)
            ));
        }
    }
}

fn render_pyi_struct(out: &mut String, s: &StructBinding) {
    out.push('\n');
    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {}:\n", s.name));
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        emit_doc(out, &field.doc, "    ");
        out.push_str(&format!(
            "    @property\n    def {}(self) -> {}: ...\n",
            field.name, py_ty
        ));
    }
}

fn render_pyi_listener(
    out: &mut String,
    module: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = module.callbacks.iter().find(|c| c.name == l.event_callback) else {
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = wrapper_name(
        &module.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    let unregister_name = wrapper_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );
    out.push('\n');
    emit_doc(out, &l.doc, "");
    out.push_str(&format!(
        "def {register_name}(callback: {}) -> int: ...\n",
        py_callable_hint(&cb.params)
    ));
    out.push_str(&format!(
        "def {unregister_name}(listener_id: int) -> None: ...\n"
    ));
}

fn render_pyi_function(
    out: &mut String,
    module_name: &str,
    f: &FnBinding,
    strip_module_prefix: bool,
) {
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, py_type_hint(&p.ty)))
        .collect();
    let ret = f
        .ret
        .as_ref()
        .map(py_type_hint)
        .unwrap_or_else(|| "None".into());
    let async_kw = if f.is_async { "async " } else { "" };
    out.push('\n');
    emit_doc(out, &f.doc, "");
    out.push_str(&format!(
        "{async_kw}def {}({}) -> {}: ...\n",
        func_name,
        params.join(", "),
        ret
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
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
            errors: None,
            modules: vec![],
        }
    }

    #[test]
    fn generator_name_is_python() {
        assert_eq!(Generator::name(&PythonGenerator), "python");
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
        assert!(
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string call: {py}"
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
    fn struct_class_with_getters() {
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
                        default: None,
                    },
                    StructField {
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(py.contains("class Contact:"), "missing class: {py}");
        assert!(
            py.contains("def __init__(self, _ptr: int)"),
            "missing __init__: {py}"
        );
        assert!(
            py.contains("self._ptr = _ptr"),
            "missing _ptr assignment: {py}"
        );
        assert!(py.contains("def __del__(self)"), "missing __del__: {py}");
        assert!(
            py.contains("weaveffi_contacts_Contact_destroy"),
            "missing destroy call: {py}"
        );
        assert!(
            py.contains("def name(self) -> str:"),
            "missing name getter: {py}"
        );
        assert!(
            py.contains("weaveffi_contacts_Contact_get_name"),
            "missing name getter C call: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string in getter: {py}"
        );
        assert!(
            py.contains("def age(self) -> int:"),
            "missing age getter: {py}"
        );
        assert!(
            py.contains("weaveffi_contacts_Contact_get_age"),
            "missing age getter C call: {py}"
        );
    }

    #[test]
    fn python_builder_generated() {
        let api = Api {
            version: "0.4.0".into(),
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
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        PythonGenerator
            .generate(&api, out, &PythonConfig::default())
            .unwrap();
        let py = std::fs::read_to_string(out.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            py.contains("class ContactBuilder"),
            "missing builder class: {py}"
        );
        assert!(py.contains("def with_name("), "missing with_name: {py}");
        assert!(py.contains("def with_age("), "missing with_age: {py}");
        assert!(py.contains("def build("), "missing build: {py}");
        // Build is FFI-backed: it calls the C create symbol, checks the
        // error, and wraps the returned handle. Unset fields default to zero
        // values rather than raising.
        assert!(
            py.contains("_fn = _lib.weaveffi_contacts_Contact_create"),
            "missing create call: {py}"
        );
        assert!(
            py.contains("return Contact(_result)"),
            "missing handle wrap: {py}"
        );
        assert!(
            py.contains("self._name: str = \"\"") && py.contains("self._age: int = 0"),
            "missing zero defaults: {py}"
        );
        assert!(
            !py.contains("requires FFI backing"),
            "stub must be gone: {py}"
        );
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
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
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
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("-> \"Contact\":"),
            "missing struct return hint: {py}"
        );
        assert!(
            py.contains("ctypes.c_void_p"),
            "missing void_p for struct: {py}"
        );
        assert!(
            py.contains("return Contact(_result)"),
            "missing struct wrapping: {py}"
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
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for optional: {py}"
        );
        assert!(
            py.contains("ctypes.byref(ctypes.c_int32(id)) if id is not None else None"),
            "missing optional param conversion: {py}"
        );
        assert!(py.contains("return None"), "missing None return path: {py}");
        assert!(
            py.contains("return _result[0]"),
            "missing pointer deref: {py}"
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
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("-> Optional[str]:"),
            "missing optional str return: {py}"
        );
        assert!(
            py.contains("return _bytes_to_string(_result)"),
            "missing _bytes_to_string for optional string: {py}"
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
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(py.contains("ids: List[int]"), "missing list param: {py}");
        assert!(py.contains("-> List[int]:"), "missing list return: {py}");
        assert!(
            py.contains("ctypes.c_int32 * len(ids)"),
            "missing ctypes array creation: {py}"
        );
        assert!(
            py.contains("_out_len"),
            "missing out_len for list return: {py}"
        );
        assert!(
            py.contains("for _i in range(_out_len.value)"),
            "missing list iteration: {py}"
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
        assert!(
            py.contains("list(scores.keys())"),
            "missing keys extraction: {py}"
        );
        assert!(py.contains("_out_keys"), "missing out_keys: {py}");
        assert!(py.contains("_out_values"), "missing out_values: {py}");
    }

    #[test]
    fn struct_optional_string_getter() {
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("def email(self) -> Optional[str]:"),
            "missing optional getter: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string in optional getter: {py}"
        );
    }

    #[test]
    fn struct_enum_field_getter() {
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("def role(self) -> \"Role\":"),
            "missing enum getter: {py}"
        );
        assert!(
            py.contains("return Role(_result)"),
            "missing enum wrapping in getter: {py}"
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
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
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
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
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

        assert!(py.contains("class Contact:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));
        assert!(py.contains("def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("def first_name(self) -> str:"));
        assert!(py.contains("def email(self) -> Optional[str]:"));
        assert!(py.contains("def contact_type(self) -> \"ContactType\":"));

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
        assert_eq!(py_type_hint(&TypeRef::Struct("Foo".into())), "\"Foo\"");
        assert_eq!(py_type_hint(&TypeRef::Enum("Bar".into())), "\"Bar\"");
        assert_eq!(py_type_hint(&TypeRef::TypedHandle("Foo".into())), "\"Foo\"");
        // Cross-module references (resolved to a qualified IR name) must still
        // annotate the bare *local* class, which is the only symbol that exists
        // in the generated module.
        assert_eq!(
            py_type_hint(&TypeRef::TypedHandle("kv.Store".into())),
            "\"Store\"",
            "qualified typed handle must annotate the local class name"
        );
        assert_eq!(
            py_type_hint(&TypeRef::Struct("kv.Store".into())),
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
            py_ctypes_scalar(&TypeRef::Struct("X".into())),
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
                returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Item".into())))),
                doc: None,
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
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("-> List[\"Item\"]:"),
            "missing list struct return: {py}"
        );
        assert!(
            py.contains("Item(_result[_i])"),
            "missing struct wrapping in list: {py}"
        );
    }

    #[test]
    fn struct_bytes_field_getter() {
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("def data(self) -> bytes:"),
            "missing bytes getter: {py}"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "missing out_len in bytes getter: {py}"
        );
        assert!(
            py.contains("_result[:_out_len.value]"),
            "missing bytes slice: {py}"
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
                        default: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "metadata".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
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
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
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
        assert!(
            pyi.contains("    def id(self) -> int: ..."),
            "missing id property: {pyi}"
        );
        assert!(
            pyi.contains("    def name(self) -> str: ..."),
            "missing name property: {pyi}"
        );
        assert!(
            pyi.contains("    def email(self) -> Optional[str]: ..."),
            "missing email property: {pyi}"
        );
        assert!(
            pyi.contains("    def tags(self) -> List[str]: ..."),
            "missing tags property: {pyi}"
        );
        assert!(
            pyi.contains("    def metadata(self) -> Dict[str, int]: ..."),
            "missing metadata property: {pyi}"
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
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

        assert!(py.contains("class Contact:"), "missing class decl");
        assert!(
            py.contains("\"\"\"A contact record\"\"\""),
            "missing doc: {py}"
        );
        assert!(py.contains("def __init__(self, _ptr: int) -> None:"));
        assert!(py.contains("self._ptr = _ptr"));
        assert!(py.contains("def __del__(self) -> None:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));

        assert!(py.contains("@property\n    def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("_fn.restype = ctypes.c_int64"));

        assert!(py.contains("@property\n    def first_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_first_name"));

        assert!(py.contains("@property\n    def last_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_last_name"));

        assert!(py.contains("@property\n    def email(self) -> Optional[str]:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_email"));
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "find_contact".into(),
                    params: vec![],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
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
        assert!(
            py.contains("ctypes.byref(ctypes.c_int32(key)) if key is not None else None"),
            "missing optional i32 conversion"
        );
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for optional i32"
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
            py.contains("_string_to_bytes(prefix)"),
            "missing optional _string_to_bytes"
        );

        assert!(
            py.contains("-> Optional[\"Contact\"]:"),
            "missing Optional struct return"
        );
        assert!(
            py.contains("if _result is None:\n        return None\n    return Contact(_result)"),
            "missing optional struct None check"
        );

        assert!(
            py.contains("-> Optional[bool]:"),
            "missing Optional[bool] return"
        );
        assert!(
            py.contains("return bool(_result[0])"),
            "missing optional bool deref"
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_items".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Item".into())))),
                    doc: None,
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
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

        assert!(py.contains("ids: List[int]"), "missing List[int] param");
        assert!(
            py.contains("(ctypes.c_int32 * len(ids))(*ids)"),
            "missing list-to-array conversion"
        );
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for list param"
        );
        assert!(py.contains("ctypes.c_size_t"), "missing size_t for length");

        assert!(
            py.contains("-> List[str]:"),
            "missing List[str] return: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result[_i]) for _i in range(_out_len.value)"),
            "missing string list _bytes_to_string: {py}"
        );

        assert!(
            py.contains("-> List[\"Item\"]:"),
            "missing List struct return"
        );
        assert!(
            py.contains("Item(_result[_i]) for _i in range(_out_len.value)"),
            "missing struct wrapping in list"
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
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

        assert!(
            py.contains("settings: Dict[str, int]"),
            "missing Dict param hint"
        );
        assert!(
            py.contains("list(settings.keys())"),
            "missing keys extraction"
        );
        assert!(
            py.contains("_settings_vals = [settings[_k] for _k in _settings_keys]"),
            "missing values extraction"
        );
        assert!(
            py.contains("ctypes.c_char_p * len(_settings_keys)"),
            "missing key array creation"
        );
        assert!(
            py.contains("ctypes.c_int32 * len(_settings_vals)"),
            "missing value array creation"
        );

        assert!(
            py.contains("-> Dict[str, int]:"),
            "missing Dict return hint"
        );
        assert!(
            py.contains("_out_keys = ctypes.POINTER(ctypes.c_char_p)()"),
            "missing out_keys init"
        );
        assert!(
            py.contains("_out_values = ctypes.POINTER(ctypes.c_int32)()"),
            "missing out_values init"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "missing out_len init"
        );
        assert!(
            py.contains("if not _out_keys or not _out_values:"),
            "missing empty map check"
        );
        assert!(
            py.contains("_bytes_to_string(_out_keys[_i]): _out_values[_i]"),
            "missing map comprehension"
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
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "scores".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
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
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            modules: vec![],
        }]);

        let pyi = render_pyi_module(&api, true, "weaveffi.yml");

        assert!(pyi.contains("from enum import IntEnum"));
        assert!(pyi.contains("from typing import Callable, Dict, Iterator, List, Optional"));

        assert!(pyi.contains("class ContactType(IntEnum):"));
        assert!(pyi.contains("    Personal: int"));
        assert!(pyi.contains("    Work: int"));
        assert!(pyi.contains("    Other: int"));

        assert!(pyi.contains("class Contact:"));
        assert!(pyi.contains("    def id(self) -> int: ..."));
        assert!(pyi.contains("    def first_name(self) -> str: ..."));
        assert!(pyi.contains("    def email(self) -> Optional[str]: ..."));
        assert!(pyi.contains("    def tags(self) -> List[str]: ..."));
        assert!(pyi.contains("    def scores(self) -> Dict[str, int]: ..."));

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
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
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
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
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

        assert!(py.contains("class Contact:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));
        assert!(py.contains("@property\n    def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("@property\n    def first_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_first_name"));
        assert!(py.contains("@property\n    def last_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_last_name"));
        assert!(py.contains("@property\n    def email(self) -> Optional[str]:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_email"));
        assert!(py.contains("@property\n    def contact_type(self) -> \"ContactType\":"));
        assert!(py.contains("weaveffi_contacts_Contact_get_contact_type"));
        assert!(py.contains("return ContactType(_result)"));

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
        assert!(py.contains("return Contact(_result)"));

        assert!(py.contains("def list_contacts() -> List[\"Contact\"]:"));
        assert!(py.contains("weaveffi_contacts_list_contacts"));
        assert!(py.contains("Contact(_result[_i]) for _i in range(_out_len.value)"));

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

        let no_strip = PythonConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_python_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir2, &no_strip).unwrap();

        let py2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            py2.contains("def contacts_create_contact("),
            "default should use module-prefixed name: {py2}"
        );

        let pyi2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.pyi")).unwrap();
        assert!(
            pyi2.contains("def contacts_create_contact("),
            "pyi default should use module-prefixed name: {pyi2}"
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
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true, "weaveffi.yml");
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
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true, "weaveffi.yml");
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
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
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
                    default: None,
                }],
                builder: false,
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
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true, "weaveffi.yml");
        assert!(
            pyi.contains("Dict[\"Color\", \"Contact\"]"),
            "should contain enum-keyed map type: {pyi}"
        );
    }

    #[test]
    fn python_typed_handle_type() {
        let api = Api {
            version: "0.4.0".into(),
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
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("contact: \"Contact\""),
            "TypedHandle should use class type not int: {py}"
        );
        assert!(
            py.contains("contact._ptr"),
            "TypedHandle call arg should extract ._ptr: {py}"
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
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
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
        let err_pos = body
            .find("_check_error(_err)")
            .expect("_check_error should appear in find_contact");
        let contact_pos = body
            .find("return Contact(_result)")
            .expect("return Contact(_result) should appear in find_contact");
        assert!(
            err_pos < contact_pos,
            "_check_error(_err) should precede return Contact(_result): {body}"
        );

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
            contact_class.contains("def __del__(self)"),
            "Contact should define __del__: {contact_class}"
        );
        assert!(
            contact_class.contains("_destroy"),
            "Contact.__del__ should call _destroy: {contact_class}"
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");

        assert!(
            py.contains("if _result is None:\n        return None"),
            "optional struct return should null-check before wrap: {py}"
        );
        let none_check = py
            .find("if _result is None:\n        return None")
            .expect("null-check block");
        let wrap = py
            .find("return Contact(_result)")
            .expect("Contact(_result) wrap");
        assert!(
            wrap > none_check,
            "Contact(_result) should appear after null check: {py}"
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
            code.contains("def _fetch_data_sync(id: int) -> str:"),
            "should have sync helper: {code}"
        );
        assert!(
            code.contains("async def fetch_data(id: int) -> str:"),
            "should have async wrapper: {code}"
        );
        assert!(
            code.contains("asyncio.get_event_loop()"),
            "should use get_event_loop: {code}"
        );
        assert!(
            code.contains("run_in_executor(None, _fetch_data_sync, id)"),
            "should use run_in_executor with sync fn and args: {code}"
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

    /// `ctypes.CFUNCTYPE` instances pin the C trampoline; `_cb` is held alive
    /// in the local frame for the lifetime of the synchronous helper, which
    /// blocks on `_ev.wait()` until the C callback fires. The `try/finally`
    /// around `_state` mutation ensures `_ev.set()` always runs, releasing
    /// the wait and letting `_cb` drop together with the helper frame.
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
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let code = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        let pin_count = code.matches("_cb = _cb_type(_cb_impl)").count();
        let wait_count = code.matches("_ev.wait()").count();
        let set_count = code.matches("_ev.set()").count();
        assert_eq!(
            pin_count, 1,
            "expected one `_cb = _cb_type(_cb_impl)` per async fn, got {pin_count}: {code}"
        );
        assert_eq!(
            wait_count, set_count,
            "every `_ev.wait()` must be matched by an `_ev.set()` in finally: wait={wait_count} set={set_count}: {code}"
        );
        assert!(
            code.contains("finally:\n            _ev.set()"),
            "_ev.set() must be in a finally block: {code}"
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
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let stubs = render_pyi_module(&api, true, "weaveffi.yml");
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
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
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
                    returns: Some(TypeRef::Struct("types.Name".into())),
                    doc: None,
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
                modules: vec![],
            },
        ]);

        let code = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        let stubs = render_pyi_module(&api, true, "weaveffi.yml");

        assert!(
            code.contains("Name(_result)"),
            "cross-module return should construct Name, not types.Name: {code}"
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
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
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
        let pyi = render_pyi_module(&api, true, "weaveffi.yml");
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
            py_type_hint(&TypeRef::Iterator(Box::new(TypeRef::Struct(
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
            modules: vec![],
        }]);
        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("ListItemsIterator"),
            "should reference iterator type name: {py}"
        );
        assert!(
            py.contains("_next"),
            "should call _next for iteration: {py}"
        );
        assert!(
            py.contains("_destroy"),
            "should call _destroy for cleanup: {py}"
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
            deprecated: Some("Use add_v2 instead".into()),
            since: Some("0.1.0".into()),
        }])]);
        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("warnings.warn(\"Use add_v2 instead\", DeprecationWarning, stacklevel=2)"),
            "missing deprecation warning: {py}"
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
                builder: false,
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
        assert!(py.contains("\"\"\"Stable id\"\"\""), "{py}");
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
}
