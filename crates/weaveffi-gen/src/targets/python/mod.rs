//! Python (`ctypes`) binding generator.
//!
//! Emits a pip-installable package of `ctypes` bindings and `.pyi` type
//! stubs over the C ABI (revision 3): distribution `{name}`, import package
//! `{prefix}`, implementation module `{prefix}/{prefix}.py`. Every C
//! prototype is bound once at import time, after the load-time ABI and
//! contract-checksum check. Records and rich enums are dataclasses crossing
//! the boundary as value buffers; interfaces are reference-counted wrapper
//! classes with `close()` and a `__del__` backstop; callback interfaces are
//! abstract base classes backed by one static vtable of `ctypes`
//! trampolines; async functions are coroutines whose cancellation cancels
//! the native call; `iter<T>` returns are lazy Python iterators.

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

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{BindingModel, ModuleBinding};
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::python::calls::{
    render_bindings, render_callable, render_callback_interface, FnScope,
};
use crate::targets::python::entities::{
    render_enum, render_error, render_interface, render_struct, root_error_name,
};
use crate::targets::python::package::{
    render_packaged_readme, render_packaged_setup_py, render_py_typed, render_pyproject_toml,
    render_readme,
};
use crate::targets::python::runtime::{render_runtime, RuntimeNames};
use crate::targets::python::stubs::render_pyi_module;

/// Per-target configuration for [`PythonGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PythonConfig {
    /// The distribution name published to PyPI (default: the package
    /// identity's `name`).
    pub package_name: Option<String>,
    /// The import package (default: the identity's C prefix, which is
    /// always a valid Python identifier). Also names the package directory
    /// and the implementation module inside it.
    pub import_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Python function names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            import_name: None,
            strip_module_prefix: true,
            input_basename: None,
        }
    }
}

impl PythonConfig {
    /// The input IDL basename embedded in generated file headers.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// The names one generation run uses, resolved from the identity and the
/// configuration.
struct Names {
    /// The distribution name.
    dist: String,
    /// The import package and implementation module name.
    import: String,
}

impl Names {
    fn new(id: &Identity, config: &PythonConfig) -> Self {
        Self {
            dist: config
                .package_name
                .clone()
                .unwrap_or_else(|| id.name.clone()),
            import: config
                .import_name
                .clone()
                .unwrap_or_else(|| id.prefix.clone()),
        }
    }
}

/// The shared context every rendering function receives.
pub(crate) struct Gen<'a> {
    /// The C symbol prefix (stripped from binding names).
    pub prefix: &'a str,
    /// The root exception class name.
    pub root_error: &'a str,
    /// Whether free functions drop their module-path prefix.
    pub strip_module_prefix: bool,
}

/// Python backend: emits a pip-installable package of `ctypes` bindings and
/// `.pyi` type stubs over the C ABI exposed by the underlying cdylib.
pub struct PythonGenerator;

impl PythonGenerator {
    /// Render the implementation module: the runtime, then every module's
    /// members in canonical order (error domain, enums, records, callback
    /// interfaces, interfaces, functions).
    fn render_py_source(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        config: &PythonConfig,
        file_name: &str,
    ) -> String {
        let id = api.identity();
        let root_error = root_error_name(model, &id.pascal_name());
        let g = Gen {
            prefix: &model.prefix,
            root_error: &root_error,
            strip_module_prefix: config.strip_module_prefix,
        };
        let mut out = render_prelude(CommentStyle::Hash, config.input_basename());
        render_runtime(
            &mut out,
            model,
            &RuntimeNames {
                identity: id,
                error: &root_error,
            },
        );
        for m in &model.modules {
            out.push_str(&format!("\n\n# === Module: {} ===\n", m.dot_path));
            emit_module(&mut out, &g, m);
        }
        out.push('\n');
        out.push_str(&render_trailer(CommentStyle::Hash, file_name));
        out
    }

    /// The `__init__.py` re-exporting the implementation module.
    fn render_init(&self, import: &str, config: &PythonConfig) -> String {
        let hash = CommentStyle::Hash;
        format!(
            "{}from .{import} import *  # noqa: F401,F403\n\n{}",
            render_prelude(hash, config.input_basename()),
            render_trailer(hash, "__init__.py"),
        )
    }

    /// The implementation module and its stub, keyed by file name.
    fn package_sources(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        config: &PythonConfig,
        names: &Names,
    ) -> [(String, String); 4] {
        let py = format!("{}.py", names.import);
        let pyi = format!("{}.pyi", names.import);
        let root_error = root_error_name(model, &api.identity().pascal_name());
        [
            (
                "__init__.py".into(),
                self.render_init(&names.import, config),
            ),
            (py.clone(), self.render_py_source(api, model, config, &py)),
            (
                pyi.clone(),
                render_pyi_module(
                    model,
                    &root_error,
                    config.strip_module_prefix,
                    config.input_basename(),
                    &pyi,
                ),
            ),
            ("py.typed".into(), render_py_typed(config.input_basename())),
        ]
    }
}

/// Emit every member of `m` in canonical order. Callback interfaces precede
/// interfaces because interface members take them as parameters.
fn emit_module(out: &mut String, g: &Gen<'_>, m: &ModuleBinding) {
    if let Some(e) = m.error.as_ref().filter(|e| e.declared_here) {
        render_error(out, g, m, e);
    }
    for e in &m.enums {
        render_enum(out, e);
    }
    for s in &m.structs {
        render_struct(out, s);
    }
    for cb in &m.callback_interfaces {
        render_callback_interface(out, g, cb);
    }
    for i in &m.interfaces {
        render_interface(out, g, m, i);
    }
    for f in &m.functions {
        render_bindings(out, g, f, m.error.as_ref(), &f.name, false);
        render_callable(out, g, m, f, FnScope::Free);
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

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let names = Names::new(api.identity(), config);
        let input_basename = config.input_basename();
        let dir = out_dir.join("python");
        let pkg_dir = dir.join(&names.import);
        let mut files: Vec<OutputFile> = self
            .package_sources(api, model, config, &names)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(pkg_dir.join(name), contents))
            .collect();
        files.push(OutputFile::new(
            dir.join("pyproject.toml"),
            render_pyproject_toml(api.identity(), &names.dist, &names.import, input_basename),
        ));
        files.push(OutputFile::new(
            dir.join("README.md"),
            render_readme(api.identity(), &names.dist, &names.import, input_basename),
        ));
        files
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let id = api.identity();
        let names = Names::new(id, config);
        let input_basename = config.input_basename();
        let sources = self.package_sources(api, model, config, &names);
        let setup_py =
            render_packaged_setup_py(&names.dist, &id.version, &names.import, input_basename);
        let pyproject = render_pyproject_toml(id, &names.dist, &names.import, input_basename);

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
            let pkg_dir = tree.join(&names.import);
            for (name, contents) in &sources {
                files.push(PackagedFile::text(pkg_dir.join(name), contents.clone()));
            }
            // Bundled under the file name the loader looks for.
            files.push(PackagedFile::copy(
                pkg_dir.join(platform.lib_filename(&id.library)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                tree.join("pyproject.toml"),
                pyproject.clone(),
            ));
            files.push(PackagedFile::text(tree.join("setup.py"), setup_py.clone()));
            files.push(PackagedFile::text(
                tree.join("README.md"),
                render_packaged_readme(&names.dist, &names.import, platform, tag, input_basename),
            ));
        }
        Some(files)
    }
}
