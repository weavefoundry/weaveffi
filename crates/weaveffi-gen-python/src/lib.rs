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

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod stubs;
mod types;

#[cfg(test)]
mod tests;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{
    BindingModel, CallbackBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    ListenerBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{render_callable, render_callback_type, render_listener, FnScope};
use crate::entities::{render_enum, render_error, render_interface, render_struct};
use crate::package::{
    render_packaged_readme, render_packaged_setup_py, render_pyproject_toml, render_readme,
    render_setup_py,
};
use crate::runtime::{py_loader_packaged, render_preamble, PY_LOADER_ORIGINAL};
use crate::stubs::render_pyi_module;

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
        api: &ResolvedApi,
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
        api: &ResolvedApi,
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

/// Render the `weaveffi.py` module source. Thin wrapper over the shared
/// [`LanguageBackend::emit_members`] walk (via
/// [`PythonGenerator::render_py_source`]); retained for direct use in tests.
#[cfg(test)]
fn render_python_module(
    api: &ResolvedApi,
    strip_module_prefix: bool,
    prefix: &str,
    input_basename: &str,
) -> String {
    let model = BindingModel::build(api, prefix);
    PythonGenerator.render_py_source(&model, strip_module_prefix, input_basename)
}
