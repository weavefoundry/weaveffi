//! Python (`ctypes`) binding generator for WeaveFFI.
//!
//! Emits a pip-installable package containing `ctypes`-based bindings and
//! `.pyi` type stubs over the C ABI (revision 2). Records and rich enums are
//! dataclasses crossing the boundary as value buffers; interfaces are
//! reference-counted wrapper classes with `close()` and a `__del__` backstop;
//! callback interfaces are abstract base classes the consumer subclasses,
//! backed by one static vtable of `ctypes` trampolines per interface; async
//! functions surface as `async def` wrappers and `iter<T>` returns as lazy
//! Python iterators. Implements [`LanguageBackend`]; the shared driver bridges
//! it into the generator pipeline.
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
#[cfg(test)]
mod tests;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    ModuleBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{render_callable, render_callback_interface, FnScope};
use crate::entities::{render_enum, render_error, render_interface, render_struct};
use crate::package::{
    render_packaged_readme, render_packaged_setup_py, render_py_typed, render_pyproject_toml,
    render_readme, render_setup_py,
};
use crate::runtime::{
    py_loader_packaged, render_feature_runtime, render_preamble, PY_LOADER_ORIGINAL,
};
use crate::stubs::render_pyi_module;

/// Per-target configuration for [`PythonGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    /// [`LanguageBackend::emit_members`] walk over every module.
    fn render_py_source(&self, model: &BindingModel, config: &PythonConfig) -> String {
        let mut out = render_prelude(CommentStyle::Hash, config.input_basename());
        render_preamble(&mut out);
        render_feature_runtime(&mut out, model.has_async(), model.has_callback_interfaces());
        // The model is a flat, pre-order list of modules, each carrying its
        // joined symbol path, the same traversal order the recursive walk
        // produced.
        for m in &model.modules {
            out.push_str(&format!("\n\n# === Module: {} ===", m.path));
            self.emit_members(&mut out, m, config);
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

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
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

    fn render_callback_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        config: &Self::Config,
    ) {
        render_callback_interface(out, module, cb, config.prefix());
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
                self.render_py_source(model, config),
            ),
            OutputFile::new(
                pkg_dir.join("weaveffi.pyi"),
                render_pyi_module(model, config.strip_module_prefix, input_basename),
            ),
            OutputFile::new(pkg_dir.join("py.typed"), render_py_typed(input_basename)),
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
        let py_source = self.render_py_source(model, config).replace(
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
        // Wheels exist only for the platforms that have a wheel platform tag;
        // a binary for any other platform (Android, wasm32) has no wheel to
        // land in and is skipped.
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let Some(tag) = platform.python_platform_tag() else {
                continue;
            };
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
            files.push(PackagedFile::text(
                pkg_dir.join("py.typed"),
                render_py_typed(input_basename),
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
                render_packaged_readme(&package, &import_name, platform, tag, input_basename),
            ));
        }
        Some(files)
    }
}
