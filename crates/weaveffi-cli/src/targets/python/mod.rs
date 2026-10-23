//! Python (`ctypes`) binding generator.
//!
//! Emits a pip-installable package of `ctypes` bindings over the C ABI
//! (revision 5): distribution `{name}`, import package `{prefix}`, and one
//! fully annotated implementation module `{prefix}/{prefix}.py` with a
//! `py.typed` marker (Python 3.10 or later). Every C prototype is bound once
//! at import time, after the load-time ABI revision and contract-table
//! check. Records and rich enums are frozen, slotted dataclasses crossing
//! the boundary as value buffers; optional scalars and numeric lists cross
//! directly (`bool` flag plus value, and typed arrays); interfaces are
//! reference-counted wrapper classes with `close()` and a `__del__`
//! backstop; callback interfaces are abstract base classes backed by one
//! static vtable of `ctypes` trampolines; async functions are coroutines
//! whose cancellation cancels the native call; `iter<T>` returns are lazy
//! `NativeIterator[T]` objects.

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

use std::collections::{HashMap, HashSet};

use crate::codegen::docs::{ApiNames, Doc, IdentKind};
use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{Artifact, ArtifactKind, PackageContext, PackagedFile, WheelMeta};
use crate::platform::{glibc_requirement, Os};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8PathBuf;
use heck::ToSnakeCase;
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::errors::type_name;
use weaveffi_model::model::{Model, ModuleBinding};
use weaveffi_model::pkg::Identity;
use weaveffi_model::plan::ErrorStrategy;

use crate::targets::python::callbacks::render_callback_interface;
use crate::targets::python::calls::{render_bindings, render_callable, FnScope};
use crate::targets::python::codec::render_composite_codecs;
use crate::targets::python::entities::{
    render_enum, render_error, render_interface, render_struct,
};
use crate::targets::python::package::{
    render_py_typed, render_pyproject_toml, render_readme, render_wheel_readme,
};
use crate::targets::python::runtime::render_runtime;
use crate::targets::python::types::{
    py_field, py_member_name, py_name, py_object_member, py_variant,
};

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
    /// (default `>=3.10`, the oldest Python the generated code runs on).
    pub requires_python: String,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            name: None,
            import_name: None,
            requires_python: ">=3.10".into(),
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

/// Python's builtin exception names ending in `Error`. A derived code class
/// name (`Timeout` -> `TimeoutError`) never takes one, since a star import
/// of the package would shadow the builtin.
const PY_BUILTIN_ERRORS: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "EOFError",
    "EnvironmentError",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "IOError",
    "ImportError",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotADirectoryError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PermissionError",
    "ProcessLookupError",
    "PythonFinalizationError",
    "RecursionError",
    "ReferenceError",
    "RuntimeError",
    "SyntaxError",
    "SystemError",
    "TabError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "ValueError",
    "ZeroDivisionError",
];

/// The public classes the runtime itself defines.
const RUNTIME_CLASSES: &[&str] = &["LibraryLoadError", "NativeIterator"];

/// The shared context every rendering function receives: the model, the
/// resolved exception class names, and the identifier index docs are
/// rewritten through.
pub(crate) struct Gen<'a> {
    /// The model being rendered.
    pub model: &'a Model,
    /// The C symbol prefix (stripped from binding names).
    pub prefix: &'a str,
    /// The root exception class every declared error derives from: `Error`
    /// (so consumers write `except kvstore.Error`), unless the API declares
    /// that name, then `{PascalName}Error`, then `{PascalName}BaseError`.
    pub root_error: String,
    /// The unchecked `RuntimeError` subclass a failed call that declares no
    /// errors raises: `InternalError`, unless the API declares that name.
    pub trap_error: String,
    /// Every error domain with its shared type names, by declaring module.
    errors: Vec<ErrorTable<'a>>,
    /// Each error code's module-level class name, by code name (code names
    /// are globally unique).
    code_classes: HashMap<String, String>,
    /// Every identifier the API declares, for rewriting doc text.
    names: ApiNames,
}

impl<'a> Gen<'a> {
    fn new(model: &'a Model) -> Self {
        let errors = errors::tables(model, "Error");
        let mut taken: HashSet<String> = RUNTIME_CLASSES.iter().map(|s| s.to_string()).collect();
        for m in &model.modules {
            taken.extend(m.enums.iter().map(|e| e.name.clone()));
            taken.extend(m.structs.iter().map(|s| s.name.clone()));
            taken.extend(m.interfaces.iter().map(|i| i.name.clone()));
            taken.extend(m.callback_interfaces.iter().map(|c| c.name.clone()));
        }
        taken.extend(errors.iter().map(|t| t.type_name.clone()));
        // A code class is the shared `{Code}Error` name, or, when that's a
        // builtin or another declaration, `{DomainStem}{Code}Error`.
        let mut code_classes = HashMap::new();
        for t in &errors {
            for row in &t.codes {
                let mut class = row.type_name.clone();
                if PY_BUILTIN_ERRORS.contains(&class.as_str()) || taken.contains(&class) {
                    class = format!("{}{class}", type_name(&t.domain.name, ""));
                }
                taken.insert(class.clone());
                code_classes.insert(row.code.name.clone(), class);
            }
        }
        let pascal = model.identity.pascal_name();
        let free = |candidates: [String; 3]| {
            let fallback = format!("{}Root", candidates[2]);
            candidates
                .into_iter()
                .find(|n| !taken.contains(n))
                .unwrap_or(fallback)
        };
        let root_error = free([
            "Error".into(),
            format!("{pascal}Error"),
            format!("{pascal}BaseError"),
        ]);
        let trap_error = free([
            "InternalError".into(),
            format!("{pascal}InternalError"),
            format!("{pascal}RuntimeError"),
        ]);
        Self {
            model,
            prefix: model.prefix(),
            root_error,
            trap_error,
            errors,
            code_classes,
            names: ApiNames::new(model),
        }
    }

    /// The error domains `m` declares.
    fn errors_of(&self, m: &ModuleBinding) -> impl Iterator<Item = &ErrorTable<'a>> {
        let index = m.index;
        self.errors.iter().filter(move |t| t.module.index == index)
    }

    /// The class of the error domain named `name` (the shared type name).
    pub(crate) fn domain_class(&self, name: &str) -> &str {
        self.errors
            .iter()
            .find(|t| t.domain.name == name)
            .map(|t| t.type_name.as_str())
            .expect("a validated model declares every domain a callable throws")
    }

    /// The module-level class of the error code named `code`.
    pub(crate) fn code_class(&self, code: &str) -> &str {
        &self.code_classes[code]
    }

    /// The factory building the exception of a failed call that throws the
    /// domain `name`: `_{snake class}_from`.
    pub(crate) fn domain_factory(&self, name: &str) -> String {
        format!("_{}_from", self.domain_class(name).to_snake_case())
    }

    /// The factory an out-err slot of a callable with error strategy `error`
    /// is raised through: the domain's typed factory, the root error's
    /// (`throws: any`), or the unchecked trap's.
    pub(crate) fn raise_factory(&self, error: &ErrorStrategy) -> String {
        match error {
            ErrorStrategy::Domain(name) => self.domain_factory(name),
            ErrorStrategy::Untyped => format!("_{}_from", self.root_error.to_snake_case()),
            ErrorStrategy::Trap => "_trap_from".into(),
        }
    }

    /// The `Raises` entries of a callable with error strategy `error`.
    pub(crate) fn raises(&self, error: &ErrorStrategy) -> Vec<(String, String)> {
        match error {
            ErrorStrategy::Domain(name) => vec![(
                self.domain_class(name).to_string(),
                "If the call fails with one of the domain's codes.".into(),
            )],
            ErrorStrategy::Untyped => vec![(self.root_error.clone(), "If the call fails.".into())],
            ErrorStrategy::Trap => vec![],
        }
    }

    /// The Python spelling of an API identifier named in backticks in doc
    /// text, or `None` to keep it as written.
    fn spell(&self, ident: &str) -> Option<String> {
        Some(match self.names.kind(ident)? {
            IdentKind::Function | IdentKind::CallbackMethod => py_member_name(ident),
            IdentKind::Member => py_object_member(ident),
            IdentKind::Param => py_name(ident),
            IdentKind::Field => py_field(ident),
            IdentKind::ErrorDomain => type_name(ident, "Error"),
            IdentKind::ErrorCode => self.code_classes.get(ident)?.clone(),
            IdentKind::Module | IdentKind::Type | IdentKind::Variant => return None,
        })
    }

    /// Doc text in Python spellings, or `None` when there's none.
    pub(crate) fn text(&self, doc: &Option<String>) -> Option<String> {
        Doc::new(doc, &None).text(|i| self.spell(i))
    }

    /// Doc text followed by a `Deprecated: ...` paragraph, in Python
    /// spellings.
    pub(crate) fn doc(&self, doc: &Option<String>, deprecated: &Option<String>) -> Option<String> {
        Doc::new(doc, deprecated).with_deprecation(|i| self.spell(i))
    }

    /// The deprecation message in Python spellings, for the
    /// `DeprecationWarning` a deprecated callable issues.
    pub(crate) fn deprecation(&self, deprecated: &Option<String>) -> Option<String> {
        Doc::new(&None, deprecated).deprecation(|i| self.spell(i))
    }
}

/// Python backend: emits a pip-installable package of fully annotated
/// `ctypes` bindings over the C ABI exposed by the underlying cdylib.
pub struct PythonGenerator {
    config: PythonConfig,
}

impl From<PythonConfig> for PythonGenerator {
    fn from(config: PythonConfig) -> Self {
        Self { config }
    }
}

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

/// Emit every member of `m` in canonical order: error domains, enums,
/// records, callback interfaces (which interface members take as
/// parameters), interfaces, and functions.
fn emit_module(w: &mut CodeWriter, g: &Gen<'_>, m: &ModuleBinding) {
    for t in g.errors_of(m) {
        render_error(w, g, t);
    }
    for e in &m.enums {
        render_enum(w, g, e);
    }
    for s in &m.structs {
        render_struct(w, g, s);
    }
    for cb in &m.callback_interfaces {
        render_callback_interface(w, g, cb);
    }
    for i in &m.interfaces {
        render_interface(w, g, i);
    }
    for f in &m.functions {
        render_bindings(w, g, f, false);
        render_callable(w, g, f, FnScope::Free);
    }
}

/// Emit `__all__`: the public names, in declaration order, so a star import
/// (the package's `__init__.py`) re-exports the API and nothing else.
fn render_all(w: &mut CodeWriter, g: &Gen<'_>) {
    let mut names = vec![g.root_error.clone(), g.trap_error.clone()];
    names.extend(RUNTIME_CLASSES.iter().map(|s| s.to_string()));
    for m in &g.model.modules {
        for t in g.errors_of(m) {
            names.push(t.type_name.clone());
            names.extend(
                t.codes
                    .iter()
                    .map(|c| g.code_class(&c.code.name).to_string()),
            );
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

impl Target for PythonGenerator {
    fn name(&self) -> &'static str {
        "python"
    }

    /// The import package, where the loader looks for a bundled library
    /// first.
    fn dev_bundle_dir(&self, model: &Model) -> Option<Utf8PathBuf> {
        Some(Utf8PathBuf::from(
            Names::new(&model.identity, &self.config).import,
        ))
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &["README.md", "__init__.py", "py.typed", "pyproject.toml"]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let config = &self.config;
        let names = Names::new(&model.identity, config);
        let dir = Utf8PathBuf::new();
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
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
        let id = &model.identity;
        let names = Names::new(id, config);
        let sources = self.package_sources(model, &names);
        let mut artifacts = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let glibc = if platform.os() == Os::Linux {
                glibc_requirement(&nb.library)?
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
        Ok(artifacts)
    }
}
