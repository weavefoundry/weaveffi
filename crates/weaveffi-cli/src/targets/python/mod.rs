//! Python (`ctypes`) binding generator.
//!
//! Emits a pip-installable package of `ctypes` bindings over the C ABI
//! (revision 4): distribution `{name}`, import package `{prefix}`, and one
//! fully annotated implementation module `{prefix}/{prefix}.py` with a
//! `py.typed` marker. Every C prototype is bound once at import time, after
//! the load-time ABI revision and contract-table check. Records and rich
//! enums are dataclasses crossing the boundary as value buffers; interfaces
//! are reference-counted wrapper classes with `close()` and a `__del__`
//! backstop; callback interfaces are abstract base classes backed by one
//! static vtable of `ctypes` trampolines; async functions are coroutines
//! whose cancellation cancels the native call; `iter<T>` returns are lazy
//! Python iterators.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
#[cfg(test)]
mod tests;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{Artifact, ArtifactKind, PackageContext, PackagedFile, WheelMeta};
use crate::platform::{glibc_requirement, Os};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{Model, ModuleBinding};
use weaveffi_model::pkg::Identity;

use crate::targets::python::callbacks::render_callback_interface;
use crate::targets::python::calls::{render_bindings, render_callable, FnScope};
use crate::targets::python::codec::render_composite_codecs;
use crate::targets::python::entities::{
    py_code_class_name, render_enum, render_error, render_interface, render_struct,
    root_error_name, trap_error_name,
};
use crate::targets::python::package::{
    render_py_typed, render_pyproject_toml, render_readme, render_wheel_readme,
};
use crate::targets::python::runtime::render_runtime;
use crate::targets::python::types::{py_member_name, py_variant};

/// Per-target configuration for [`PythonGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PythonConfig {
    /// The distribution name published to PyPI (default: the package
    /// identity's `name`).
    pub name: Option<String>,
    /// The import package (default: the identity's C prefix, which is
    /// always a valid Python identifier). Also names the package directory
    /// and the implementation module inside it.
    pub import_name: Option<String>,
    /// The `requires-python` specifier of the project and its wheels
    /// (default `>=3.9`).
    pub requires_python: String,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            name: None,
            import_name: None,
            requires_python: ">=3.9".into(),
        }
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
            dist: config.name.clone().unwrap_or_else(|| id.name.clone()),
            import: config
                .import_name
                .clone()
                .unwrap_or_else(|| id.prefix.clone()),
        }
    }
}

/// The metadata of the wheel for one platform `tag`.
fn wheel_meta(id: &Identity, names: &Names, config: &PythonConfig, tag: String) -> WheelMeta {
    let mut urls = Vec::new();
    if let Some(homepage) = &id.homepage {
        urls.push(("Homepage".to_string(), homepage.clone()));
    }
    if let Some(repository) = &id.repository {
        urls.push(("Repository".to_string(), repository.clone()));
    }
    WheelMeta {
        name: names.dist.clone(),
        version: id.version.clone(),
        platform: tag,
        summary: id.description_or_default(),
        requires_python: Some(config.requires_python.clone()),
        license: id.license.clone(),
        author: (!id.authors.is_empty()).then(|| id.authors.join(", ")),
        urls,
        readme: Some(render_wheel_readme(id, &names.dist, &names.import)),
    }
}

/// The shared context every rendering function receives.
pub(crate) struct Gen<'a> {
    /// The model being rendered.
    pub model: &'a Model,
    /// The C symbol prefix (stripped from binding names).
    pub prefix: &'a str,
    /// The root exception class name (see [`root_error_name`]).
    pub root_error: String,
    /// The unchecked trap exception class name (see [`trap_error_name`]).
    pub trap_error: String,
}

impl<'a> Gen<'a> {
    fn new(model: &'a Model) -> Self {
        let pascal = model.identity.pascal_name();
        Self {
            model,
            prefix: model.prefix(),
            root_error: root_error_name(model, &pascal),
            trap_error: trap_error_name(model, &pascal),
        }
    }
}

/// Python backend: emits a pip-installable package of fully annotated
/// `ctypes` bindings over the C ABI exposed by the underlying cdylib.
pub struct PythonGenerator;

impl PythonGenerator {
    /// Render the implementation module: the runtime, every module's members
    /// in canonical order (error domain, enums, records, callback
    /// interfaces, interfaces, functions), the composite codecs, and
    /// `__all__`.
    fn render_py_source(&self, model: &Model, file_name: &str) -> String {
        let g = Gen::new(model);
        let mut w = CodeWriter::four_space();
        w.raw(render_prelude(CommentStyle::Hash));
        render_runtime(&mut w, &g);
        for m in &model.modules {
            w.blank().blank();
            w.line(format!("# === Module: {} ===", m.dot_path));
            emit_module(&mut w, &g, m);
        }
        render_composite_codecs(&mut w, model);
        render_all(&mut w, &g);
        w.blank();
        w.raw(render_trailer(CommentStyle::Hash, file_name));
        w.finish()
    }

    /// The `__init__.py` re-exporting the implementation module's public
    /// names (its `__all__`).
    fn render_init(&self, import: &str) -> String {
        let hash = CommentStyle::Hash;
        format!(
            "{}from .{import} import *  # noqa: F401,F403\n\n{}",
            render_prelude(hash),
            render_trailer(hash, "__init__.py"),
        )
    }

    /// The import package's files, keyed by file name.
    fn package_sources(&self, model: &Model, names: &Names) -> [(String, String); 3] {
        let py = format!("{}.py", names.import);
        [
            ("__init__.py".into(), self.render_init(&names.import)),
            (py.clone(), self.render_py_source(model, &py)),
            ("py.typed".into(), render_py_typed()),
        ]
    }
}

/// Emit every member of `m` in canonical order. Callback interfaces precede
/// interfaces because interface members take them as parameters.
fn emit_module(w: &mut CodeWriter, g: &Gen<'_>, m: &ModuleBinding) {
    let error = g.model.error_domain(m);
    if let Some(e) = m.errors.as_ref() {
        render_error(w, g, m, e);
    }
    for e in &m.enums {
        render_enum(w, e);
    }
    for s in &m.structs {
        render_struct(w, s);
    }
    for cb in &m.callback_interfaces {
        render_callback_interface(w, g, cb, error.map(|e| e.type_name.as_str()));
    }
    for i in &m.interfaces {
        render_interface(w, g, m, i);
    }
    for f in &m.functions {
        render_bindings(w, g, f, error, &f.name, false);
        render_callable(w, g, m, f, FnScope::Free);
    }
}

/// Emit `__all__`: the public names, in declaration order, so a star import
/// (the package's `__init__.py`) re-exports the API and nothing else.
fn render_all(w: &mut CodeWriter, g: &Gen<'_>) {
    let mut names = vec![g.root_error.clone(), g.trap_error.clone()];
    for m in &g.model.modules {
        if let Some(e) = &m.errors {
            names.push(e.type_name.clone());
            names.extend(e.codes.iter().map(|c| py_code_class_name(&c.name)));
        }
        for e in &m.enums {
            names.push(e.name.clone());
            if e.is_rich() {
                names.extend(
                    e.variants
                        .iter()
                        .map(|v| format!("{}{}", e.name, py_variant(&v.name))),
                );
            }
        }
        names.extend(m.structs.iter().map(|s| s.name.clone()));
        names.extend(m.callback_interfaces.iter().map(|c| c.name.clone()));
        names.extend(m.interfaces.iter().map(|i| i.name.clone()));
        names.extend(m.functions.iter().map(|f| py_member_name(&f.name)));
    }
    w.blank().blank();
    w.line("__all__ = [");
    w.scope(|w| {
        for n in &names {
            w.line(format!("\"{n}\","));
        }
    });
    w.line("]");
}

impl LanguageBackend for PythonGenerator {
    type Config = PythonConfig;

    fn name(&self) -> &'static str {
        "python"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let names = Names::new(&model.identity, config);
        let dir = out_dir.join("python");
        let pkg_dir = dir.join(&names.import);
        let mut files: Vec<OutputFile> = self
            .package_sources(model, &names)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(pkg_dir.join(name), contents))
            .collect();
        files.push(OutputFile::new(
            dir.join("pyproject.toml"),
            render_pyproject_toml(
                &model.identity,
                &names.dist,
                &names.import,
                &config.requires_python,
            ),
        ));
        files.push(OutputFile::new(
            dir.join("README.md"),
            render_readme(&model.identity, &names.dist, &names.import),
        ));
        files
    }

    /// One `py3-none-{platform}` wheel per desktop platform, with the
    /// library inside the import package where the loader looks first.
    /// Platforms without a wheel tag (Android, iOS, `wasm32`) are skipped.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let id = &model.identity;
        let names = Names::new(id, config);
        let sources = self.package_sources(model, &names);
        let mut artifacts = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let glibc = if platform.os() == Os::Linux {
                std::fs::read(nb.library.as_std_path())
                    .map(|bytes| glibc_requirement(&bytes))
                    .unwrap_or((2, 17))
            } else {
                (2, 17)
            };
            let Some(tag) = platform.python_platform_tag(ctx.macos_deployment_target, glibc) else {
                continue;
            };
            let mut files: Vec<PackagedFile> = sources
                .iter()
                .map(|(name, contents)| {
                    PackagedFile::text(format!("{}/{name}", names.import), contents.clone())
                })
                .collect();
            // Bundled under the file name the loader looks for.
            files.push(PackagedFile::copy(
                format!("{}/{}", names.import, platform.lib_filename(&id.library)),
                nb.library.clone(),
            ));
            let meta = wheel_meta(id, &names, config, tag);
            artifacts.push(Artifact {
                path: format!("python/{}", meta.file_name()).into(),
                kind: ArtifactKind::Wheel(meta),
                files,
            });
        }
        Some(artifacts)
    }
}
