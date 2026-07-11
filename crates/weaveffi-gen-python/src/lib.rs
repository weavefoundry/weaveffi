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
use heck::{ToShoutySnakeCase, ToSnakeCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, is_c_pointer_type, pascal_case, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, ListenerBinding, ModuleBinding, ParamBinding, RichVariantBinding,
    StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`PythonGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PythonConfig {
    /// pip-installable Python package name (default `"weaveffi"`). Also
    /// determines the on-disk package directory inside `python/`.
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Python function names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the ctypes bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
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
    /// `render_python_module` wrapper so there is one assembly path.
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
        let has_async = model
            .modules
            .iter()
            .flat_map(|m| m.callables())
            .any(|f| f.is_async);
        if has_async {
            out.push_str(
                "\nimport asyncio\nimport threading\n\n\n\
                 # Pending async completion trampolines, keyed by an integer token.\n\
                 # Holding the ctypes function objects here keeps them alive until the\n\
                 # producer fires the completion callback, even when the awaiting\n\
                 # coroutine has been cancelled; each entry is removed on completion.\n\
                 _async_pending: Dict[int, object] = {}\n\
                 _async_lock = threading.Lock()\n\
                 _async_next_token = 0\n\n\n\
                 def _async_register(cb) -> int:\n    \
                     global _async_next_token\n    \
                     with _async_lock:\n        \
                         _async_next_token += 1\n        \
                         _token = _async_next_token\n        \
                         _async_pending[_token] = cb\n    \
                     return _token\n",
            );
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

    fn render_error(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        e: &ErrorBinding,
        _config: &Self::Config,
    ) {
        render_error(out, module, e);
    }

    fn render_enum(&self, out: &mut String, e: &EnumBinding, _config: &Self::Config) {
        render_enum(out, e);
    }

    fn render_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        i: &InterfaceBinding,
        _config: &Self::Config,
    ) {
        render_interface(out, module, i);
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
        render_callable(
            out,
            f,
            module.error.as_ref(),
            &FnScope::Free {
                module_path: &module.path,
                strip_module_prefix: config.strip_module_prefix,
            },
        );
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
                render_pyi_module(model, config.strip_module_prefix, input_basename),
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

    fn package(
        &self,
        api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let import_name = package.ident_name();
        let input_basename = config.input_basename();
        let hash = CommentStyle::Hash;

        // Render the binding source once with the bundled-first loader, then
        // reuse it across every per-platform wheel tree.
        let py_source = self
            .render_py_source(model, config.strip_module_prefix, input_basename)
            .replace(
                PY_LOADER_ORIGINAL,
                &py_loader_packaged(&ctx.binaries.lib_name),
            );
        let init_py = format!(
            "{}from .weaveffi import *  # noqa: F401,F403\n\n{}",
            render_prelude(hash, input_basename),
            render_trailer(hash, "__init__.py"),
        );
        let pyi = render_pyi_module(model, config.strip_module_prefix, input_basename);
        let setup_py = render_packaged_setup_py(&package, &import_name, input_basename);
        let pyproject = render_pyproject_toml(&package, &import_name, input_basename);

        let py_dir = out_dir.join("python");
        let mut files = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let tree = py_dir.join(platform.id());
            let pkg_dir = tree.join(&import_name);
            files.push(PackagedFile::text(
                pkg_dir.join("__init__.py"),
                init_py.clone(),
            ));
            files.push(PackagedFile::text(
                pkg_dir.join("weaveffi.py"),
                py_source.clone(),
            ));
            files.push(PackagedFile::text(
                pkg_dir.join("weaveffi.pyi"),
                pyi.clone(),
            ));
            files.push(PackagedFile::copy(
                pkg_dir.join(ctx.binaries.bundled_filename(platform)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                tree.join("pyproject.toml"),
                pyproject.clone(),
            ));
            files.push(PackagedFile::text(tree.join("setup.py"), setup_py.clone()));
            files.push(PackagedFile::text(
                tree.join("README.md"),
                render_packaged_readme(&package, &import_name, platform, input_basename),
            ));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(PythonGenerator);

/// The exact `_load_library` block `render_py_source` emits in `generate`
/// mode, so the packager can swap it for a bundled-first variant.
const PY_LOADER_ORIGINAL: &str = r#"def _load_library() -> ctypes.CDLL:
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
    return ctypes.CDLL(name)"#;

/// The packaged `_load_library` for `lib`: prefer the per-platform library
/// bundled next to the module, then `WEAVEFFI_LIBRARY`, then the system path.
fn py_loader_packaged(lib: &str) -> String {
    format!(
        r#"def _load_library() -> ctypes.CDLL:
    # A bundled per-platform library ships next to this module; prefer it so the
    # package works with no external setup. WEAVEFFI_LIBRARY still overrides.
    override = os.environ.get("WEAVEFFI_LIBRARY")
    if override:
        return ctypes.CDLL(override)
    here = os.path.dirname(os.path.abspath(__file__))
    system = platform.system()
    if system == "Darwin":
        name = "lib{lib}.dylib"
    elif system == "Windows":
        name = "{lib}.dll"
    else:
        name = "lib{lib}.so"
    bundled = os.path.join(here, name)
    if os.path.exists(bundled):
        return ctypes.CDLL(bundled)
    return ctypes.CDLL(name)"#
    )
}

/// Render a `setup.py` for a packaged wheel: it ships the bundled library as
/// package data and forces a non-pure (platform-tagged) wheel.
fn render_packaged_setup_py(
    package: &ResolvedPackage,
    import_name: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "setup.py");
    let name = &package.name;
    let version = &package.version;
    format!(
        r#"{prelude}from setuptools import setup
from setuptools.dist import Distribution


class _BinaryDistribution(Distribution):
    # Force a non-pure, platform-tagged wheel: the package bundles a native
    # shared library, so it is not portable across platforms.
    def has_ext_modules(self):
        return True


setup(
    name="{name}",
    version="{version}",
    packages=["{import_name}"],
    package_data={{"{import_name}": ["*.so", "*.dylib", "*.dll"]}},
    include_package_data=True,
    distclass=_BinaryDistribution,
)

{trailer}"#,
    )
}

/// README for a packaged per-platform Python wheel tree.
fn render_packaged_readme(
    package: &ResolvedPackage,
    import_name: &str,
    platform: weaveffi_core::platform::Platform,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let tag = platform.python_platform_tag();
    format!(
        r#"{prelude}# {name} (Python, {plat})

Auto-generated Python bindings with the native library bundled for `{plat}`.
The library loads automatically; no external setup is required.

## Build the wheel

```bash
python -m build --wheel
```

Tag the resulting wheel for this platform with `{tag}` (for example via
`wheel tags --platform-tag {tag} dist/*.whl`) before publishing.

## Usage

```python
from {import_name} import *
```

{trailer}"#,
        plat = platform.id(),
    )
}

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
        // Records, rich enums, and interfaces all cross the ABI as opaque
        // object pointers.
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::Interface(_) => "ctypes.c_void_p",
        TypeRef::Enum(_) => "ctypes.c_int32",
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) | TypeRef::Iterator(_) => {
            "ctypes.c_void_p"
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The `ctypes` element type for a slot the wrapper *owns and must free*
/// (list returns, map buffers, iterator `next` slots). Owned string elements
/// stay raw `c_void_p` addresses so the pointer survives long enough to be
/// copied and passed to `weaveffi_free_string`; a `c_char_p` slot would be
/// auto-converted to `bytes` by ctypes, losing the pointer and leaking the
/// producer allocation.
fn py_owned_elem_scalar(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "ctypes.c_void_p",
        _ => py_ctypes_scalar(ty),
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
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::Interface(name) => {
            format!("\"{}\"", local_type_name(name))
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
                format!(
                    "ctypes.POINTER(ctypes.POINTER({}))",
                    py_owned_elem_scalar(k)
                ),
                format!(
                    "ctypes.POINTER(ctypes.POINTER({}))",
                    py_owned_elem_scalar(v)
                ),
                "ctypes.POINTER(ctypes.c_size_t)".into(),
            ],
        );
    }
    match ty {
        // Iterator constructors return the opaque iterator handle; the `_next`
        // signature is emitted separately by the iterator code path.
        TypeRef::Iterator(_) => ("ctypes.c_void_p".into(), vec![]),
        // An owned string return keeps its raw address so the wrapper can
        // copy it and pass it back to `weaveffi_free_string`; a `c_char_p`
        // restype would be auto-converted to `bytes`, losing the pointer.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("ctypes.c_void_p".into(), vec![]),
        TypeRef::List(inner) => (
            format!("ctypes.POINTER({})", py_owned_elem_scalar(inner)),
            vec!["ctypes.POINTER(ctypes.c_size_t)".into()],
        ),
        TypeRef::Optional(inner) if is_c_pointer_type(inner) => py_return_info(inner),
        _ => {
            let r = abi::lower_return(ty, "");
            let out = r.out_params.iter().map(|p| py_ctype(&p.ty)).collect();
            (py_ctype(&r.ret), out)
        }
    }
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

/// Append the success branch of an async completion trampoline: convert the
/// borrowed `result` slots into the idiomatic value and store it in
/// `_state["val"]`. Borrowed buffers (strings, bytes, arrays, map buffers)
/// are copied and never freed; owned object pointers (records, rich enums,
/// interfaces) are adopted by their wrapper class, which destroys them.
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
            // `result` arrives as `bytes` (ctypes copies `c_char_p` callback
            // arguments), so decoding is already a deep copy of the borrowed
            // producer buffer. The producer frees it; the wrapper must not.
            out.push_str(&format!(
                "{ind}_state[\"val\"] = _bytes_to_string(result) or \"\"\n"
            ));
        }
        Some(TypeRef::Enum(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}_state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::Record(name))
        | Some(TypeRef::RichEnum(name))
        | Some(TypeRef::TypedHandle(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if result is None:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"err\"] = WeaveFFIError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
        }
        // A returned interface transfers ownership of a new object reference;
        // wrap it without re-running the class's FFI constructor.
        Some(TypeRef::Interface(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if result is None:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"err\"] = WeaveFFIError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = {name}._from_ptr(result)\n"
            ));
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            // Copy the borrowed buffer; the producer owns and frees it.
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = b\"\"\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _n = int(result_len)\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = bytes(result[:_n])\n"));
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
                        // Borrowed; `bytes` conversion already copied it.
                        out.push_str(&format!(
                            "{ind}_state[\"val\"] = _bytes_to_string(result)\n"
                        ));
                    }
                    TypeRef::Record(name)
                    | TypeRef::RichEnum(name)
                    | TypeRef::TypedHandle(name) => {
                        let name = local_type_name(name);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
                    }
                    TypeRef::Interface(name) => {
                        let name = local_type_name(name);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!(
                            "{ind}    _state[\"val\"] = {name}._from_ptr(result)\n"
                        ));
                    }
                    TypeRef::Bytes | TypeRef::BorrowedBytes => {
                        // Copy the borrowed buffer; the producer frees it.
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _n = int(result_len)\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = bytes(result[:_n])\n"));
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
        Some(TypeRef::Named(_)) => unreachable!("unresolved type reference"),
    }
}

/// Render the callback-driven body of an `async def` wrapper at the body
/// indent `ind`.
///
/// The wrapper creates a future on the running `asyncio` loop, builds the
/// `CFUNCTYPE` completion trampoline for the launcher's callback typedef,
/// pins it in `_async_pending` until completion, invokes the launcher (which
/// returns immediately), and awaits the future. The trampoline runs on an
/// arbitrary producer thread: it copies borrowed result buffers before
/// returning (owned object pointers are adopted instead), then resolves the
/// future via `call_soon_threadsafe`. A throwing callable maps the completion
/// error through the module domain's factory (from `error`); a non-throwing
/// one traps with the generic `WeaveFFIError`. When `has_self` is set (an
/// instance method), the launcher receives `self._ptr` as its leading
/// argument.
fn render_async_ffi_call_body(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    ind: &str,
    has_self: bool,
) {
    let CallShape::Async(a) = &f.shape else {
        unreachable!("render_async_ffi_call_body requires an async call shape");
    };
    let err_expr = match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => format!("{}(_code, _msg)", py_error_factory_name(eb)),
        _ => "WeaveFFIError(_code, _msg)".to_string(),
    };

    out.push_str(&format!("{ind}_fn = _lib.{}\n", a.launch.symbol));
    out.push_str(&format!("{ind}_loop = asyncio.get_running_loop()\n"));
    out.push_str(&format!("{ind}_fut = _loop.create_future()\n"));

    let trailing = py_async_cb_trailing_fields(&f.ret);
    let mut cb_param_list: Vec<String> = vec!["context".into(), "err".into()];
    cb_param_list.extend(trailing.iter().map(|(n, _)| n.clone()));
    let cb_params_joined = cb_param_list.join(", ");

    out.push('\n');
    out.push_str(&format!("{ind}def _cb_impl({cb_params_joined}):\n"));
    out.push_str(&format!(
        "{ind}    # Fires exactly once, on a producer thread: convert (copying\n"
    ));
    out.push_str(&format!(
        "{ind}    # borrowed buffers) here, then hop back to the event loop.\n"
    ));
    out.push_str(&format!(
        "{ind}    _state = {{\"err\": None, \"val\": None}}\n"
    ));
    out.push_str(&format!("{ind}    if err and err.contents.code != 0:\n"));
    out.push_str(&format!("{ind}        _code = err.contents.code\n"));
    out.push_str(&format!(
        "{ind}        _msg = err.contents.message.decode(\"utf-8\") if err.contents.message else \"\"\n"
    ));
    out.push_str(&format!(
        "{ind}        _lib.weaveffi_error_clear(ctypes.byref(err.contents))\n"
    ));
    out.push_str(&format!("{ind}        _state[\"err\"] = {err_expr}\n"));
    out.push_str(&format!("{ind}    else:\n"));
    append_async_success_handler(out, &f.ret, &format!("{ind}        "));
    out.push('\n');
    out.push_str(&format!("{ind}    def _resolve():\n"));
    out.push_str(&format!("{ind}        _async_pending.pop(_token, None)\n"));
    out.push_str(&format!(
        "{ind}        # A cancelled future must not be resolved.\n"
    ));
    out.push_str(&format!("{ind}        if _fut.cancelled():\n"));
    out.push_str(&format!("{ind}            return\n"));
    out.push_str(&format!("{ind}        if _state[\"err\"] is not None:\n"));
    out.push_str(&format!(
        "{ind}            _fut.set_exception(_state[\"err\"])\n"
    ));
    out.push_str(&format!("{ind}        else:\n"));
    out.push_str(&format!(
        "{ind}            _fut.set_result(_state[\"val\"])\n"
    ));
    out.push('\n');
    out.push_str(&format!("{ind}    _loop.call_soon_threadsafe(_resolve)\n"));
    out.push('\n');

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
    out.push_str(&format!(
        "{ind}_token = _async_register(_cb)  # pinned until completion\n"
    ));

    let mut argtypes: Vec<String> = Vec::new();
    if has_self {
        argtypes.push("ctypes.c_void_p".into());
    }
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
        for line in py_param_conversion(&py_name(&p.name), &p.ty, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    let mut call_args: Vec<String> = Vec::new();
    if has_self {
        call_args.push("self._ptr".into());
    }
    for p in &f.params {
        call_args.extend(py_param_call_args(&py_name(&p.name), &p.ty));
    }
    if f.cancellable {
        call_args.push("None".into());
    }
    call_args.push("_cb".into());
    call_args.push("None".into());

    out.push_str(&format!("{ind}_fn({})\n", call_args.join(", ")));
    if f.ret.is_some() {
        out.push_str(&format!("{ind}return await _fut\n"));
    } else {
        out.push_str(&format!("{ind}await _fut\n"));
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
/// each parameter that has a `doc:` value, and a `Raises` section naming the
/// domain error type when `raises` is set (throwing callables only). Skips
/// entirely when there is nothing to document.
fn emit_fn_docstring(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    raises: Option<&str>,
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
    if trimmed_doc.is_none() && documented_params.is_empty() && raises.is_none() {
        return;
    }
    out.push_str(indent);
    out.push_str("\"\"\"");
    let mut has_content = false;
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
        has_content = true;
    } else {
        out.push('\n');
    }
    if !documented_params.is_empty() {
        if has_content {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("Parameters\n");
        out.push_str(indent);
        out.push_str("----------\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("{} : {}\n", py_name(&p.name), first));
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
        has_content = true;
    }
    if let Some(domain) = raises {
        if has_content {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("Raises\n");
        out.push_str(indent);
        out.push_str("------\n");
        out.push_str(indent);
        out.push_str(domain);
        out.push('\n');
        out.push_str(indent);
        out.push_str("    If the call reports one of the domain's error codes.\n");
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
# The free helpers take raw addresses (`c_void_p`) so wrappers can release
# owned producer allocations they hold as plain integers or typed pointers.
_lib.weaveffi_free_string.argtypes = [ctypes.c_void_p]
_lib.weaveffi_free_string.restype = None
_lib.weaveffi_free_bytes.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
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


def _take_string(ptr) -> Optional[str]:
    """Copy an owned C string (a raw address) and free the producer buffer."""
    if not ptr:
        return None
    _s = ctypes.string_at(ptr).decode("utf-8")
    _lib.weaveffi_free_string(ptr)
    return _s
"#,
    );
}

// ── Typed errors ──

/// The snake_case stem of a domain's generated helpers: `KvError` becomes
/// `kv_error`, naming `_kv_error_from` and `_check_kv_error`. Domain type
/// names are globally unique (validated), so the helpers can't collide.
fn py_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `_{stem}_from`: builds the domain exception matching an ABI code.
fn py_error_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}_from", py_error_stem(eb))
}

/// `_check_{stem}`: raises the domain exception for a non-zero out-err slot.
fn py_error_checker_name(eb: &ErrorBinding) -> String {
    format!("_check_{}", py_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through: the module
/// domain's typed checker when the callable throws, the generic
/// `_check_error` (plain `WeaveFFIError`, panics and marshalling failures
/// only) otherwise.
fn py_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => py_error_checker_name(eb),
        _ => "_check_error".to_string(),
    }
}

/// Escape a string for embedding in a double-quoted Python literal.
fn py_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The Python class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`), matching the new samples'
/// already-Pascal code names. Each class is also attached to its domain class
/// (`KvError.KeyNotFound`), which stays unambiguous even if two domains
/// declare codes with the same name.
fn py_code_class_name(name: &str) -> String {
    weaveffi_core::errors::pascal(name)
}

/// Render one module's declared error domain: a base exception named after
/// the domain (subclassing the generic `WeaveFFIError`), one exception
/// subclass per code carrying its stable `CODE` and default message, the
/// code-to-class table, and the factory/checker helpers throwing wrappers
/// route their out-err slots through. Each code class is also attached to
/// the domain class, so consumers can catch `KvError.KeyNotFound`.
fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = py_error_factory_name(eb);
    let checker = py_error_checker_name(eb);
    let table = format!("_{}_CODES", eb.type_name.to_shouty_snake_case());

    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {domain}(WeaveFFIError):"));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Base exception for the `{}` module's error domain.\"\"\"",
            module.path
        ));
    });

    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        let message = py_str_literal(&c.message);
        w.blank().blank();
        w.line(format!("class {class}({domain}):"));
        w.indent();
        let mut doc = String::new();
        emit_docstring(&mut doc, &c.doc, &w.indent_str());
        if doc.is_empty() {
            emit_docstring(&mut doc, &Some(c.message.clone()), &w.indent_str());
        }
        w.raw(doc);
        w.blank();
        w.line(format!("CODE = {}", c.value));
        w.blank();
        w.line(format!(
            "def __init__(self, message: str = \"{message}\") -> None:"
        ));
        w.scope(|w| {
            w.line(format!("super().__init__({}, message)", c.value));
        });
        w.dedent();
    }

    // Scoped aliases: `except KvError.KeyNotFound` stays unambiguous even if
    // another domain declares a code with the same name.
    w.blank().blank();
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        w.line(format!("{domain}.{class} = {class}"));
    }

    w.blank().blank();
    w.line(format!("{table}: Dict[int, type] = {{"));
    w.scope(|w| {
        for c in &eb.codes {
            let class = py_code_class_name(&c.name);
            w.line(format!("{}: {class},", c.value));
        }
    });
    w.line("}");

    w.blank().blank();
    w.line(format!(
        "def {factory}(code: int, message: str) -> WeaveFFIError:"
    ));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Build the {domain} subclass matching `code`, or a generic"
        ));
        w.line("WeaveFFIError for codes outside the domain (panics, marshalling).\"\"\"");
        w.line(format!("_cls = {table}.get(code)"));
        w.line("if _cls is None:");
        w.scope(|w| {
            w.line("return WeaveFFIError(code, message)");
        });
        w.line("return _cls(message) if message else _cls()");
    });

    w.blank().blank();
    w.line(format!("def {checker}(err: _WeaveFFIErrorStruct) -> None:"));
    w.scope(|w| {
        w.line("if err.code != 0:");
        w.scope(|w| {
            w.line("code = err.code");
            w.line("message = err.message.decode(\"utf-8\") if err.message else \"\"");
            w.line("_lib.weaveffi_error_clear(ctypes.byref(err))");
            w.line(format!("raise {factory}(code, message)"));
        });
    });

    out.push_str(&w.finish());
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // Rich (algebraic) enums cross the ABI as opaque objects, so they are
    // emitted as wrapper classes (like structs), not plain `IntEnum`s.
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {}(IntEnum):", e.name));
    w.indent();
    let mut doc = String::new();
    emit_docstring(&mut doc, &e.doc, "    ");
    w.raw(doc);
    for v in &e.variants {
        if let Some(d) = &v.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    w.line(format!("# {}", line));
                }
            }
        }
        w.line(format!("{} = {}", v.name, v.value));
    }
    out.push_str(&w.finish());
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
    let ret_ty = py_type_hint(&TypeRef::Record(s.name.clone()));
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

// ── Interfaces ──

/// Render one interface as an opaque-object wrapper class, following the
/// struct wrapper's ownership pattern: the class owns the raw C pointer and
/// releases it exactly once, calling the interface's destroy symbol from
/// `__del__`. A constructor named `new` becomes `__init__`; every other
/// constructor becomes a `@classmethod` factory; methods pass `self._ptr` as
/// the leading C argument; statics are `@staticmethod`s. `_from_ptr` wraps a
/// pointer the producer already handed over (a C return value) without
/// re-running the FFI constructor.
fn render_interface(out: &mut String, module: &ModuleBinding, i: &InterfaceBinding) {
    let error = module.error.as_ref();

    // `_...Iterator` helpers are module-level classes; emit them ahead of the
    // wrapper so nothing nests inside the class body. The interface name
    // qualifies the helper so two interfaces can share a method name.
    for m in i.methods.iter().chain(i.statics.iter()) {
        if let (Some(TypeRef::Iterator(inner)), CallShape::Iterator(it)) = (&m.ret, &m.shape) {
            let checker = py_checker_name(m, error);
            render_iterator_class(
                out,
                &it.iter_tag,
                &format!("{}_{}", i.name, m.name),
                inner,
                &checker,
            );
        }
    }

    out.push_str(&format!("\n\nclass {}:\n", i.name));
    emit_docstring(out, &i.doc, "    ");

    out.push_str(&format!(
        "\n    @classmethod\n    def _from_ptr(cls, ptr) -> \"{}\":",
        i.name
    ));
    out.push_str("\n        _obj = cls.__new__(cls)");
    out.push_str("\n        _obj._ptr = ptr");
    out.push_str("\n        return _obj");

    let new_ctor = i.constructors.iter().find(|c| c.name == "new");
    if let Some(c) = new_ctor {
        render_callable(out, c, error, &FnScope::Init);
    } else {
        // No canonical constructor: expose the same raw-pointer `__init__`
        // the struct wrappers use, so factories stay the only public path.
        out.push_str("\n\n    def __init__(self, _ptr: int) -> None:");
        out.push_str("\n        self._ptr = _ptr\n");
    }

    let destroy = &i.destroy_symbol;
    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{destroy}.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!("\n            _lib.{destroy}.restype = None"));
    out.push_str(&format!("\n            _lib.{destroy}(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    for c in &i.constructors {
        if c.name != "new" {
            render_callable(out, c, error, &FnScope::Factory);
        }
    }
    for m in &i.methods {
        render_callable(out, m, error, &FnScope::Method { class: &i.name });
    }
    for s in &i.statics {
        render_callable(out, s, error, &FnScope::Static { class: &i.name });
    }
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
    let mut w = CodeWriter::four_space();
    w.blank().blank();
    let mut doc = String::new();
    emit_doc(&mut doc, &c.doc, "");
    w.raw(doc);
    w.line(format!(
        "# Callback type {}: {}",
        c.name,
        py_callable_hint(&c.params)
    ));
    w.line(format!(
        "_CFUNC_{} = ctypes.CFUNCTYPE({})",
        c.c_fn_type,
        parts.join(", ")
    ));
    out.push_str(&w.finish());
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
        // duration of the call, so opaque pointers (including interface
        // references) pass through raw rather than being wrapped in an owning
        // class whose __del__ would free them.
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => n.into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({n})"),
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                format!("bytes({n}_ptr[:{n}_len]) if {n}_ptr else None")
            }
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => n.into(),
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
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
    )
    .to_snake_case();
    let unregister_name = wrapper_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_snake_case();
    let cfunc = format!("_CFUNC_{}", cb.c_fn_type);
    let ind = "    ";

    let mut w = CodeWriter::four_space();

    // register_{listener}(callback) -> int
    w.blank().blank();
    w.line(format!(
        "def {register_name}(callback: {}) -> int:",
        py_callable_hint(&cb.params)
    ));
    w.indent();
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
    let mut doc = String::new();
    emit_docstring(&mut doc, &Some(reg_doc), ind);
    w.raw(doc);

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
    w.line(format!("def _trampoline({}):", tramp_params.join(", ")));
    w.scope(|w| {
        w.line(format!("callback({})", call_args.join(", ")));
    });
    w.line(format!("_cfunc = {cfunc}(_trampoline)"));
    w.line(format!("_fn = _lib.{}", l.register_symbol));
    w.line(format!("_fn.argtypes = [{cfunc}, ctypes.c_void_p]"));
    w.line("_fn.restype = ctypes.c_uint64");
    w.line("_listener_id = int(_fn(_cfunc, None))");
    w.line("_listener_refs[_listener_id] = _cfunc");
    w.line("return _listener_id");
    w.dedent();

    // unregister_{listener}(listener_id) -> None
    w.blank().blank();
    w.line(format!("def {unregister_name}(listener_id: int) -> None:"));
    w.indent();
    let mut unreg_doc = String::new();
    emit_docstring(
        &mut unreg_doc,
        &Some(format!(
            "Unregister a listener previously registered with {register_name}()."
        )),
        ind,
    );
    w.raw(unreg_doc);
    w.line(format!("_fn = _lib.{}", l.unregister_symbol));
    w.line("_fn.argtypes = [ctypes.c_uint64]");
    w.line("_fn.restype = None");
    w.line("_fn(ctypes.c_uint64(listener_id))");
    w.line("_listener_refs.pop(listener_id, None)");
    out.push_str(&w.finish());
}

fn render_getter(out: &mut String, field: &FieldBinding) {
    let getter = &field.getter_symbol;
    let py_ty = py_type_hint(&field.ty);
    let ind = "        ";

    let mut w = CodeWriter::four_space();
    w.blank().blank().indent();
    w.line("@property");
    w.line(format!("def {}(self) -> {}:", field.name, py_ty));
    w.indent();
    let mut doc = String::new();
    emit_docstring(&mut doc, &field.doc, ind);
    w.raw(doc);
    w.line(format!("_fn = _lib.{getter}"));

    let (restype, out_argtypes) = py_return_info(&field.ty);
    let mut argtypes = vec!["ctypes.c_void_p".to_string()];
    argtypes.extend(out_argtypes.iter().cloned());

    w.line(format!("_fn.argtypes = [{}]", argtypes.join(", ")));
    w.line(format!("_fn.restype = {restype}"));

    if out_argtypes.is_empty() {
        w.line("_result = _fn(self._ptr)");
    } else if let Some((k, v)) = get_map_kv(&field.ty) {
        // The locals must match the `py_return_info` argtypes exactly: owned
        // string elements stay `c_void_p` so their raw addresses survive for
        // `weaveffi_free_string` (ctypes rejects a `c_char_p` local against a
        // `POINTER(POINTER(c_void_p))` argtype).
        w.line(format!(
            "_out_keys = ctypes.POINTER({})()",
            py_owned_elem_scalar(k)
        ));
        w.line(format!(
            "_out_values = ctypes.POINTER({})()",
            py_owned_elem_scalar(v)
        ));
        w.line("_out_len = ctypes.c_size_t(0)");
        w.line("_fn(self._ptr, ctypes.byref(_out_keys), ctypes.byref(_out_values), ctypes.byref(_out_len))");
    } else {
        w.line("_out_len = ctypes.c_size_t(0)");
        w.line("_result = _fn(self._ptr, ctypes.byref(_out_len))");
    }
    out.push_str(&w.finish());

    render_return_value(out, &field.ty, ind);
}

/// How a rendered callable is scoped and spelled in the generated Python.
#[derive(Clone, Copy)]
enum FnScope<'a> {
    /// A module-level free function.
    Free {
        /// The owning module's underscore-joined path.
        module_path: &'a str,
        /// Whether the emitted name drops the module-path prefix.
        strip_module_prefix: bool,
    },
    /// An instance method on an interface wrapper: leading `self` parameter,
    /// `self._ptr` passed as the leading C argument. Carries the wrapper
    /// class name, which qualifies the member's iterator helper class.
    Method {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A `@staticmethod` member; carries the wrapper class name, which
    /// qualifies the member's iterator helper class.
    Static {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A `@classmethod` constructor factory returning a new wrapper instance.
    Factory,
    /// The canonical `new` constructor, emitted as `__init__`.
    Init,
}

impl FnScope<'_> {
    /// True for every scope rendered inside a class body (depth 1).
    fn is_member(&self) -> bool {
        !matches!(self, FnScope::Free { .. })
    }

    /// True when the C call receives `self._ptr` as its leading argument.
    fn has_self_slot(&self) -> bool {
        matches!(self, FnScope::Method { .. })
    }

    /// The owner stem of a member's iterator helper class name:
    /// `{Interface}_{member}` for members, the bare function name otherwise.
    fn iterator_owner(&self, fn_name: &str) -> String {
        match self {
            FnScope::Method { class } | FnScope::Static { class } => {
                format!("{class}_{fn_name}")
            }
            _ => fn_name.to_string(),
        }
    }

    /// Indentation depth of the `def` line (0 at module scope, 1 in a class).
    fn depth(&self) -> usize {
        usize::from(self.is_member())
    }
}

/// The Python spelling of an IDL value identifier (parameter name):
/// snake_case via heck. IDL names are usually already snake, so this is a
/// safety net for camelCase inputs.
fn py_name(name: &str) -> String {
    name.to_snake_case()
}

/// The emitted Python name for a callable in `scope`: `__init__` for the
/// canonical constructor, otherwise the snake_case member name, with the
/// module-path prefix applied to free functions when configured.
fn py_fn_name(f: &FnBinding, scope: &FnScope) -> String {
    match scope {
        FnScope::Free {
            module_path,
            strip_module_prefix,
        } => wrapper_name(module_path, &f.name, *strip_module_prefix).to_snake_case(),
        FnScope::Init => "__init__".to_string(),
        _ => f.name.to_snake_case(),
    }
}

/// Render one callable: a free function or an interface member. `error` is
/// the module's error domain (used when the callable throws); `scope` picks
/// the def spelling, receiver, indent, and result handling. Sync, async, and
/// iterator shapes all route through here so members reuse the free-function
/// marshalling paths.
fn render_callable(out: &mut String, f: &FnBinding, error: Option<&ErrorBinding>, scope: &FnScope) {
    let func_name = py_fn_name(f, scope);
    let depth = scope.depth();
    let ind = "    ".repeat(depth + 1);
    let checker = py_checker_name(f, error);
    let raises = error.filter(|_| f.throws).map(|eb| eb.type_name.as_str());

    let receiver = match scope {
        FnScope::Method { .. } | FnScope::Init => Some("self"),
        FnScope::Factory => Some("cls"),
        _ => None,
    };
    let mut params_sig: Vec<String> = Vec::new();
    if let Some(r) = receiver {
        params_sig.push(r.to_string());
    }
    params_sig.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
    );
    let ret_hint = match scope {
        FnScope::Init => "None".to_string(),
        _ => f
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".to_string()),
    };

    let is_iterator_ret = matches!(f.shape, CallShape::Iterator(_));

    // The `_...Iterator` helper class is module-level; a member's helper is
    // emitted by `render_interface` ahead of the wrapper class instead.
    if let (Some(TypeRef::Iterator(inner)), CallShape::Iterator(it)) = (&f.ret, &f.shape) {
        if !scope.is_member() {
            render_iterator_class(out, &it.iter_tag, &f.name, inner, &checker);
        }
    }

    let decorator = match scope {
        FnScope::Static { .. } => Some("@staticmethod"),
        FnScope::Factory => Some("@classmethod"),
        _ => None,
    };

    let mut w = CodeWriter::four_space().with_depth(depth);
    w.blank().blank();
    if let Some(d) = decorator {
        w.line(d);
    }
    w.line(format!(
        "{}def {}({}) -> {}:",
        if f.is_async { "async " } else { "" },
        func_name,
        params_sig.join(", "),
        ret_hint
    ));
    w.indent();

    // An iterator-returning wrapper documents the streaming contract next to
    // whatever the IDL says about the function itself.
    let doc = if is_iterator_ret {
        let streaming = "Returns a lazy iterator: each step pulls one element from the \
                         producer. Exhaust or close() the iterator to release its native \
                         handle (garbage collection also releases it)."
            .to_string();
        Some(match &f.doc {
            Some(d) => format!("{}\n\n{streaming}", d.trim()),
            None => streaming,
        })
    } else {
        f.doc.clone()
    };
    let mut fdoc = String::new();
    emit_fn_docstring(&mut fdoc, &doc, &f.params, &ind, raises);
    w.raw(fdoc);

    // Set before any fallible statement so `__del__` never sees a
    // half-constructed instance when the constructor raises.
    if matches!(scope, FnScope::Init) {
        w.line("self._ptr = None");
    }

    if let Some(msg) = &f.deprecated {
        w.line("import warnings");
        w.line(format!(
            "warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)",
            msg.replace('"', "\\\"")
        ));
    }

    if f.is_async {
        // The async FFI call body is rendered at the function-body indent and
        // spliced in verbatim.
        let mut body = String::new();
        render_async_ffi_call_body(&mut body, f, error, &ind, scope.has_self_slot());
        w.raw(body);
        out.push_str(&w.finish());
    } else {
        let sym = match &f.shape {
            CallShape::Sync(abi) => abi.symbol.as_str(),
            CallShape::Iterator(it) => it.launch.symbol.as_str(),
            CallShape::Async(_) => unreachable!("async handled above"),
        };
        w.line(format!("_fn = _lib.{sym}"));

        let mut argtypes: Vec<String> = Vec::new();
        if scope.has_self_slot() {
            argtypes.push("ctypes.c_void_p".into());
        }
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

        w.line(format!("_fn.argtypes = [{}]", argtypes.join(", ")));
        w.line(format!("_fn.restype = {restype}"));

        for p in &f.params {
            for line in py_param_conversion(&py_name(&p.name), &p.ty, &ind) {
                w.raw(&line).raw("\n");
            }
        }

        w.line("_err = _WeaveFFIErrorStruct()");

        let is_map_ret = f.ret.as_ref().and_then(get_map_kv).is_some();
        let has_out_len = !out_ret_argtypes.is_empty() && !is_map_ret;

        if let Some((k, v)) = f.ret.as_ref().and_then(get_map_kv) {
            w.line(format!(
                "_out_keys = ctypes.POINTER({})()",
                py_owned_elem_scalar(k)
            ));
            w.line(format!(
                "_out_values = ctypes.POINTER({})()",
                py_owned_elem_scalar(v)
            ));
            w.line("_out_len = ctypes.c_size_t(0)");
        } else if has_out_len {
            w.line("_out_len = ctypes.c_size_t(0)");
        }

        let mut call_args: Vec<String> = Vec::new();
        if scope.has_self_slot() {
            call_args.push("self._ptr".into());
        }
        for p in &f.params {
            call_args.extend(py_param_call_args(&py_name(&p.name), &p.ty));
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
            w.line(format!("_result = {call_expr}"));
        } else {
            w.line(call_expr);
        }

        w.line(format!("{checker}(_err)"));

        match scope {
            // Constructors receive the owned pointer directly rather than
            // routing through the generic return path.
            FnScope::Init => {
                w.line("if _result is None:");
                w.scope(|w| {
                    w.line("raise WeaveFFIError(-1, \"null pointer\")");
                });
                w.line("self._ptr = _result");
                out.push_str(&w.finish());
            }
            FnScope::Factory => {
                w.line("if _result is None:");
                w.scope(|w| {
                    w.line("raise WeaveFFIError(-1, \"null pointer\")");
                });
                w.line("return cls._from_ptr(_result)");
                out.push_str(&w.finish());
            }
            _ => {
                if is_iterator_ret {
                    // Lazy: hand the caller the iterator wrapper; each step
                    // pulls one element and the wrapper owns the handle.
                    let class = py_iterator_class_name(&scope.iterator_owner(&f.name));
                    w.line(format!("return {class}(_result)"));
                    out.push_str(&w.finish());
                } else {
                    out.push_str(&w.finish());
                    if let Some(ret_ty) = &f.ret {
                        render_return_value(out, ret_ty, &ind);
                    }
                }
            }
        }
    }
}

// ── Param helpers ──

fn py_list_convert_expr(name: &str, elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[_string_to_bytes(v) for v in {name}]")
        }
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
            format!("*[v._ptr for v in {name}]")
        }
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
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
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
        // Object parameters are borrowed: pass the wrapper's raw pointer;
        // the callee never takes ownership.
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
            vec![format!("{name}._ptr")]
        }
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("_{name}_c")],
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => {
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
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

// ── Return helpers ──

fn py_read_element(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({expr})"),
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::TypedHandle(name)
        | TypeRef::Enum(name) => {
            let name = local_type_name(name);
            format!("{name}({expr})")
        }
        // Owned interface references wrap without re-running the class's FFI
        // constructor.
        TypeRef::Interface(name) => {
            format!("{}._from_ptr({expr})", local_type_name(name))
        }
        TypeRef::Bool => format!("bool({expr})"),
        _ => expr.to_string(),
    }
}

/// The read expression for one element the wrapper *owns* (list returns, map
/// buffers, iterator `next` slots): string elements are copied and released
/// through `_take_string` per [`weaveffi_core::plan::ElemFree::String`];
/// object pointers are adopted by their wrapper class, whose disposal calls
/// the `_destroy` symbol ([`weaveffi_core::plan::ElemFree::Object`]).
fn py_read_owned_element(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_take_string({expr})"),
        _ => py_read_element(expr, ty),
    }
}

fn render_return_value(out: &mut String, ty: &TypeRef, ind: &str) {
    let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
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
            w.line("return _result");
        }
        TypeRef::Bool => {
            w.line("return bool(_result)");
        }
        // Owned string: copy, then release via `weaveffi_free_string`.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("return _take_string(_result) or \"\"");
        }
        // Owned buffer: copy, then release via `weaveffi_free_bytes`.
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("if not _result:");
            w.scope(|w| {
                w.line("return b\"\"");
            });
            w.line("_val = bytes(_result[:_out_len.value])");
            w.line("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_out_len.value))");
            w.line("return _val");
        }
        // An owned object pointer is adopted by the wrapper class, whose
        // disposal calls the type's `_destroy` symbol.
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let name = local_type_name(name);
            w.line("if _result is None:");
            w.scope(|w| {
                w.line("raise WeaveFFIError(-1, \"null pointer\")");
            });
            w.line(format!("return {name}(_result)"));
        }
        // A returned interface is a new owned reference: wrap it without
        // re-running the class's FFI constructor.
        TypeRef::Interface(name) => {
            let name = local_type_name(name);
            w.line("if _result is None:");
            w.scope(|w| {
                w.line("raise WeaveFFIError(-1, \"null pointer\")");
            });
            w.line(format!("return {name}._from_ptr(_result)"));
        }
        TypeRef::Enum(name) => {
            let name = local_type_name(name);
            w.line(format!("return {name}(_result)"));
        }
        // Compound returns delegate to their own helpers, which append directly.
        TypeRef::Optional(inner) => return render_optional_return(out, inner, ind),
        TypeRef::List(inner) => return render_list_return(out, inner, ind, "[]"),
        TypeRef::Map(k, v) => return render_map_return(out, k, v, ind, "{}"),
        TypeRef::Iterator(_) => unreachable!("iterator return handled in render_function"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

fn render_optional_return(out: &mut String, inner: &TypeRef, ind: &str) {
    let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
    // Every branch but the first guards a `None` early-return, so factor the
    // shared `if not _result: return None` shape out of each arm.
    let guard_none = |w: &mut CodeWriter, none_value: &str| {
        w.line("if not _result:");
        w.scope(|w| {
            w.line(format!("return {none_value}"));
        });
    };
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("return _take_string(_result)");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            guard_none(&mut w, "None");
            w.line("_val = bytes(_result[:_out_len.value])");
            w.line("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_out_len.value))");
            w.line("return _val");
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let name = local_type_name(name);
            w.line("if _result is None:");
            w.scope(|w| {
                w.line("return None");
            });
            w.line(format!("return {name}(_result)"));
        }
        TypeRef::Interface(name) => {
            let name = local_type_name(name);
            w.line("if _result is None:");
            w.scope(|w| {
                w.line("return None");
            });
            w.line(format!("return {name}._from_ptr(_result)"));
        }
        // Optional list and map returns share the non-optional copy-and-free
        // path with a `None` empty value.
        TypeRef::List(elem) => return render_list_return(out, elem, ind, "None"),
        TypeRef::Map(k, v) => return render_map_return(out, k, v, ind, "None"),
        // Boxed optional scalars: dereference, then release the box with
        // `weaveffi_free_bytes(ptr, sizeof(T))`.
        _ if !is_c_pointer_type(inner) => {
            guard_none(&mut w, "None");
            let read = match inner {
                TypeRef::Enum(name) => format!("{}(_result[0])", local_type_name(name)),
                TypeRef::Bool => "bool(_result[0])".to_string(),
                _ => "_result[0]".to_string(),
            };
            w.line(format!("_val = {read}"));
            w.line(format!(
                "_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(ctypes.sizeof({})))",
                py_ctypes_scalar(inner)
            ));
            w.line("return _val");
        }
        _ => {
            w.line("return _result");
        }
    }
    out.push_str(&w.finish());
}

/// Render a list return: copy each element out of the producer array (owed
/// per-element releases included), then release the array buffer itself with
/// `weaveffi_free_bytes(ptr, len * sizeof(elem))`. `empty` is the value
/// returned for a null array (`[]`, or `None` for an optional list).
fn render_list_return(out: &mut String, inner: &TypeRef, ind: &str, empty: &str) {
    let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
    w.line("if not _result:");
    w.scope(|w| {
        w.line(format!("return {empty}"));
    });
    let elem = py_read_owned_element("_result[_i]", inner);
    w.line(format!("_items = [{elem} for _i in range(_out_len.value)]"));
    w.line(format!(
        "_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_out_len.value * ctypes.sizeof({})))",
        py_owned_elem_scalar(inner)
    ));
    w.line("return _items");
    out.push_str(&w.finish());
}

/// Render a map return: copy each key and value out of the parallel producer
/// arrays (owed per-element releases included), then release both arrays with
/// `weaveffi_free_bytes`. `empty` is the value returned for null buffers
/// (`{}`, or `None` for an optional map).
fn render_map_return(out: &mut String, k: &TypeRef, v: &TypeRef, ind: &str, empty: &str) {
    let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
    w.line("if not _out_keys or not _out_values:");
    w.scope(|w| {
        w.line(format!("return {empty}"));
    });
    let key_read = py_read_owned_element("_out_keys[_i]", k);
    let val_read = py_read_owned_element("_out_values[_i]", v);
    w.line(format!(
        "_map = {{{key_read}: {val_read} for _i in range(_out_len.value)}}"
    ));
    w.line(format!(
        "_lib.weaveffi_free_bytes(_out_keys, ctypes.c_size_t(_out_len.value * ctypes.sizeof({})))",
        py_owned_elem_scalar(k)
    ));
    w.line(format!(
        "_lib.weaveffi_free_bytes(_out_values, ctypes.c_size_t(_out_len.value * ctypes.sizeof({})))",
        py_owned_elem_scalar(v)
    ));
    w.line("return _map");
    out.push_str(&w.finish());
}

// ── Iterator helpers ──

fn py_read_iter_item(inner: &TypeRef) -> String {
    match inner {
        // Owned string element: copy, then `weaveffi_free_string`. The out
        // slot is a raw `c_void_p`, so the address survives to be freed.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "_take_string(_out_item.value)".into(),
        // Owned object element: adopted by the wrapper class, whose disposal
        // calls the type's `_destroy` symbol.
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::TypedHandle(name)
        | TypeRef::Enum(name) => {
            let name = local_type_name(name);
            format!("{name}(_out_item.value)")
        }
        TypeRef::Interface(name) => {
            format!("{}._from_ptr(_out_item.value)", local_type_name(name))
        }
        TypeRef::Bool => "bool(_out_item.value)".into(),
        _ => "_out_item.value".into(),
    }
}

/// The module-level helper class name for one iterator-returning callable.
/// `owner` is the function name for a free function, or
/// `{Interface}_{member}` for an interface member.
fn py_iterator_class_name(owner: &str) -> String {
    format!("_{}Iterator", pascal_case(owner))
}

/// Render the module-level `_...Iterator` helper class for one
/// iterator-returning callable, satisfying the pull contract of
/// [`weaveffi_core::plan::IteratorProtocol`]: one producer `next` call per
/// `__next__`, per-element releases after copying, and exactly one `destroy`
/// (eagerly on exhaustion, or from `close()`/`__del__` when iteration is
/// abandoned early). `checker` is the error-check helper the `next` calls
/// route their out-err slot through (typed for a throwing callable, generic
/// otherwise).
fn render_iterator_class(
    out: &mut String,
    iter_tag: &str,
    func_name: &str,
    inner: &TypeRef,
    checker: &str,
) {
    let class_name = py_iterator_class_name(func_name);
    let item_scalar = py_owned_elem_scalar(inner);
    let read_expr = py_read_iter_item(inner);

    out.push_str(&format!("\n\nclass {class_name}:"));
    out.push_str("\n    \"\"\"Lazy iterator over a producer stream: each step pulls one element");
    out.push_str("\n    across the C boundary. The native handle is released exactly once, on");
    out.push_str("\n    exhaustion, on close(), or when the iterator is garbage collected.\"\"\"");
    out.push_str("\n\n    def __init__(self, ptr):");
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
    out.push_str(&format!("\n        {checker}(_err)"));
    out.push_str("\n        if not _has:");
    out.push_str("\n            self._done = True");
    out.push_str("\n            self._destroy()");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        return {read_expr}"));

    out.push_str("\n\n    def close(self):");
    out.push_str("\n        \"\"\"Release the native iterator without draining it.\"\"\"");
    out.push_str("\n        self._done = True");
    out.push_str("\n        self._destroy()");

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

fn render_pyi_module(
    model: &BindingModel,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(
        "from enum import IntEnum\nfrom typing import Callable, Dict, Iterator, List, Optional, Type\n",
    );
    out.push_str("\nclass WeaveFFIError(Exception):\n");
    out.push_str("    code: int\n");
    out.push_str("    message: str\n");
    out.push_str("    def __init__(self, code: int, message: str) -> None: ...\n");
    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_pyi_error(&mut out, eb);
        }
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
        for i in &m.interfaces {
            render_pyi_interface(&mut out, i);
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

/// `.pyi` stub for one module's error domain: the domain base class (with
/// its scoped per-code aliases) plus a per-code subclass carrying its stable
/// `CODE`, mirroring [`render_error`].
fn render_pyi_error(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    out.push('\n');
    out.push_str(&format!("class {domain}(WeaveFFIError):\n"));
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        out.push_str(&format!("    {class}: Type[\"{class}\"]\n"));
    }
    out.push_str("    def __init__(self, code: int, message: str) -> None: ...\n");
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        out.push('\n');
        emit_doc(out, &c.doc, "");
        out.push_str(&format!("class {class}({domain}):\n"));
        out.push_str("    CODE: int\n");
        out.push_str("    def __init__(self, message: str = ...) -> None: ...\n");
    }
}

/// `.pyi` stub for one interface wrapper class: `__init__` for the canonical
/// `new` constructor, a classmethod per remaining constructor, then methods
/// and statics, mirroring [`render_interface`].
fn render_pyi_interface(out: &mut String, i: &InterfaceBinding) {
    out.push('\n');
    emit_doc(out, &i.doc, "");
    out.push_str(&format!("class {}:\n", i.name));
    let member_sig = |f: &FnBinding, receiver: Option<&str>| -> String {
        let mut params: Vec<String> = receiver.iter().map(|r| r.to_string()).collect();
        params.extend(
            f.params
                .iter()
                .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
        );
        params.join(", ")
    };
    let async_kw = |f: &FnBinding| if f.is_async { "async " } else { "" };
    if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
        out.push_str(&format!(
            "    def __init__({}) -> None: ...\n",
            member_sig(c, Some("self"))
        ));
    }
    for c in i.constructors.iter().filter(|c| c.name != "new") {
        out.push_str(&format!(
            "    @classmethod\n    def {}({}) -> \"{}\": ...\n",
            c.name.to_snake_case(),
            member_sig(c, Some("cls")),
            i.name
        ));
    }
    for m in &i.methods {
        let ret = m
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".into());
        out.push_str(&format!(
            "    {}def {}({}) -> {}: ...\n",
            async_kw(m),
            m.name.to_snake_case(),
            member_sig(m, Some("self")),
            ret
        ));
    }
    for s in &i.statics {
        let ret = s
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".into());
        out.push_str(&format!(
            "    @staticmethod\n    {}def {}({}) -> {}: ...\n",
            async_kw(s),
            s.name.to_snake_case(),
            member_sig(s, None),
            ret
        ));
    }
    if i.constructors.is_empty() && i.methods.is_empty() && i.statics.is_empty() {
        out.push_str("    ...\n");
    }
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
    )
    .to_snake_case();
    let unregister_name = wrapper_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_snake_case();
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
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix).to_snake_case();
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty)))
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
            version: "0.5.0".into(),
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

    #[test]
    fn generator_name_is_python() {
        assert_eq!(Generator::name(&PythonGenerator), "python");
    }

    fn ping_api() -> Api {
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
            interfaces: vec![],
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
        // The getter's owned string is copied and released via `_take_string`.
        assert!(
            py.contains("_take_string(_result)"),
            "missing _take_string in getter: {py}"
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
            version: "0.5.0".into(),
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
                interfaces: vec![],
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
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for optional: {py}"
        );
        assert!(
            py.contains("ctypes.byref(ctypes.c_int32(id)) if id is not None else None"),
            "missing optional param conversion: {py}"
        );
        assert!(py.contains("return None"), "missing None return path: {py}");
        // The boxed scalar is dereferenced, then its box is released.
        assert!(
            py.contains("_val = _result[0]"),
            "missing pointer deref: {py}"
        );
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes(_result, \
                 ctypes.c_size_t(ctypes.sizeof(ctypes.c_int32)))"
            ),
            "missing boxed scalar free: {py}"
        );
        assert!(py.contains("return _val"), "missing _val return: {py}");
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
        // The optional owned string is copied and released via `_take_string`,
        // which itself returns `None` for a null pointer.
        assert!(
            py.contains("return _take_string(_result)"),
            "missing _take_string for optional string: {py}"
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
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi", "weaveffi.yml");
        assert!(
            py.contains("def email(self) -> Optional[str]:"),
            "missing optional getter: {py}"
        );
        // Copied and released via `_take_string` (which handles the null case).
        assert!(
            py.contains("_take_string(_result)"),
            "missing _take_string in optional getter: {py}"
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
            interfaces: vec![],
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
        assert_eq!(py_type_hint(&TypeRef::Record("Foo".into())), "\"Foo\"");
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
            py_ctypes_scalar(&TypeRef::Record("X".into())),
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
            interfaces: vec![],
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
            interfaces: vec![],
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
        // The boxed bool is dereferenced, then its box is released.
        assert!(
            py.contains("_val = bool(_result[0])"),
            "missing optional bool deref"
        );
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes(_result, \
                 ctypes.c_size_t(ctypes.sizeof(ctypes.c_int32)))"
            ),
            "missing boxed bool free"
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
        // Each owned string element is copied and released, then the array
        // buffer itself is released.
        assert!(
            py.contains("_take_string(_result[_i]) for _i in range(_out_len.value)"),
            "missing string list _take_string: {py}"
        );
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes(_result, \
                 ctypes.c_size_t(_out_len.value * ctypes.sizeof(ctypes.c_void_p)))"
            ),
            "missing string list array free: {py}"
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
        // Owned string keys stay raw `c_void_p` addresses so each one can be
        // copied and released; `c_char_p` would lose the pointer.
        assert!(
            py.contains("_out_keys = ctypes.POINTER(ctypes.c_void_p)()"),
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
            py.contains("_take_string(_out_keys[_i]): _out_values[_i]"),
            "missing map comprehension"
        );
        // Both parallel buffers are released after copying.
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes(_out_keys, \
                 ctypes.c_size_t(_out_len.value * ctypes.sizeof(ctypes.c_void_p)))"
            ),
            "missing map key buffer free"
        );
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes(_out_values, \
                 ctypes.c_size_t(_out_len.value * ctypes.sizeof(ctypes.c_int32)))"
            ),
            "missing map value buffer free"
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
                    default: None,
                }],
                builder: false,
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
        let api = Api {
            version: "0.5.0".into(),
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
                        default: None,
                    }],
                    builder: false,
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
                    default: None,
                }],
                builder: false,
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
                    default: None,
                }],
                builder: false,
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
        assert!(
            code.contains(
                "_cb_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p, \
                           ctypes.POINTER(_WeaveFFIErrorStruct), ctypes.c_char_p)"
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
                        default: None,
                    }],
                    builder: false,
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
    fn kv_api() -> Api {
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
                    },
                    ErrorCode {
                        name: "IO_FAILURE".into(),
                        code: 2,
                        message: "io failure".into(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn python_interface_async_and_iterator_members() {
        use weaveffi_ir::ir::InterfaceDef;
        let mut api = kv_api();
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
        // A throwing async member maps errors through the domain factory.
        assert!(
            py.contains("_state[\"err\"] = _kv_error_from(_code, _msg)"),
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
            py.contains("def _kv_error_from(code: int, message: str) -> WeaveFFIError:"),
            "missing factory: {py}"
        );
        assert!(
            py.contains("def _check_kv_error(err: _WeaveFFIErrorStruct) -> None:"),
            "missing domain checker: {py}"
        );
        assert!(
            py.contains("raise _kv_error_from(code, message)"),
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
}
