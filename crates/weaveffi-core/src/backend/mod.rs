//! The language-backend framework.
//!
//! Every idiomatic WeaveFFI generator does the same three things: it walks the
//! [`BindingModel`] in a fixed order (enums → structs → callbacks → listeners
//! → functions), dispatches each function on its [`CallShape`], and writes a
//! primary source file plus a handful of package manifests. Before this module
//! existed, all eleven generators hand-rolled that walk, that dispatch, that
//! file I/O, and their own copy of the [`Generator`] glue — and they drifted.
//!
//! [`LanguageBackend`] captures the common structure as a trait whose hooks a
//! backend implements, and the free [`run`]/[`output_files`] functions plus the
//! [`impl_generator_via_backend!`](crate::impl_generator_via_backend) macro provide the shared driver. A backend
//! now owns *only* language-specific rendering: type mapping, marshalling, and
//! the exact text of each declaration. The traversal order, the call-shape
//! dispatch, the model construction, and the bridge to the object-safe
//! [`Generator`]/`DynGenerator` layer all live here, once.
//!
//! [`BindingModel`]: crate::model::BindingModel
//! [`CallShape`]: crate::model::CallShape
//! [`Generator`]: crate::codegen::Generator

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use weaveffi_ir::ir::Api;

use crate::capabilities::TargetCapabilities;
use crate::model::{
    BindingModel, CallbackBinding, EnumBinding, FnBinding, ListenerBinding, ModuleBinding,
    StructBinding,
};

/// A single generated file: its full path (under the output directory) and the
/// rendered contents. Backends return these from [`LanguageBackend::files`];
/// the driver creates parent directories and writes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: Utf8PathBuf,
    pub contents: String,
}

impl OutputFile {
    pub fn new(path: impl Into<Utf8PathBuf>, contents: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

/// An idiomatic language backend over the shared [`BindingModel`].
///
/// The single required method is [`files`](Self::files), which assembles the
/// complete output set; pair it with [`impl_generator_via_backend!`](crate::impl_generator_via_backend) to wire
/// the type into the [`Generator`](crate::codegen::Generator) trait the CLI and
/// orchestrator consume. That alone gives every backend the shared driver, the
/// [`OutputFile`] model (rendering is pure; the driver does the I/O), an
/// automatically-derived `output_files`, and one uniform `Generator` bridge.
///
/// Backends whose primary file is a straightforward per-module walk override
/// the per-entity hooks (`render_enum`, `render_struct`, `render_function`, and
/// optionally `render_callback`/`render_listener`) and call the provided
/// [`emit_members`](Self::emit_members) from inside their module scoping — that
/// is what removes the hand-rolled walk + call-shape dispatch each generator
/// used to carry. Multi-pass backends (Ruby, .NET, Node, Android) instead build
/// their own layout directly in [`files`](Self::files) and leave the hooks at
/// their no-op defaults.
///
/// Each hook renders into a `String` (matching how generators accumulate
/// output) and is responsible for emitting its own doc comments — doc-comment
/// shape varies too much between targets (docstrings, `///`, KDoc, `<summary>`)
/// to centralise here, but every backend shares
/// [`emit_doc`](crate::codegen::common::emit_doc) for the line/block flavours.
pub trait LanguageBackend: Send + Sync {
    /// Per-target, fully-typed configuration. Mirrors
    /// [`Generator::Config`](crate::codegen::Generator::Config).
    type Config: Serialize + Default + Clone + Send + Sync;

    /// Stable short name (`"swift"`, `"python"`, …): the `--target` token.
    fn name(&self) -> &'static str;

    /// The gated IDL features this backend implements (async functions,
    /// callbacks, listeners, iterators). Required — declaring capabilities
    /// explicitly is what lets the orchestrator fail loudly instead of a
    /// backend silently skipping a feature it never implemented.
    fn capabilities(&self) -> TargetCapabilities;

    /// Whether the bound config explicitly opted in to generating despite
    /// unsupported features (see
    /// [`Generator::allows_unsupported`](crate::codegen::Generator::allows_unsupported)).
    /// Backends with partial capabilities override this to read their
    /// `allow_unsupported` config flag; full-capability backends keep the
    /// `false` default.
    fn allows_unsupported(&self, config: &Self::Config) -> bool {
        let _ = config;
        false
    }

    /// The C ABI symbol prefix the producer used. The driver builds the
    /// [`BindingModel`] with it so every emitted call targets the right
    /// exported symbol. Defaults to `"weaveffi"`; override when the config
    /// carries a configurable `c_prefix`.
    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        let _ = config;
        "weaveffi"
    }

    /// Render one enum (its declaration and any helpers), including doc
    /// comments. Override when using [`emit_members`](Self::emit_members).
    fn render_enum(&self, out: &mut String, e: &EnumBinding, config: &Self::Config) {
        let _ = (out, e, config);
    }

    /// Render one struct: the wrapper type, its getters, lifecycle, and the
    /// optional builder. `module` is the owning module (for symbol paths).
    /// Override when using [`emit_members`](Self::emit_members).
    fn render_struct(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        s: &StructBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, s, config);
    }

    /// Render a module-scope callback typedef. Default: no output (most idiomatic
    /// backends express callbacks inline at the async/listener call site).
    fn render_callback(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        c: &CallbackBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, c, config);
    }

    /// Render a listener's register/unregister surface. Default: no output.
    fn render_listener(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        l: &ListenerBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, l, config);
    }

    /// Render one function. Implementations match on `f.shape` (sync / async /
    /// iterator) and emit the idiomatic wrapper plus its doc comment. Override
    /// when using [`emit_members`](Self::emit_members).
    fn render_function(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        f: &FnBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, f, config);
    }

    /// Emit every member of `module` in canonical order (enums → structs →
    /// callbacks → listeners → functions). Backends call this from within their
    /// own module scoping; overriding the per-entity hooks is what guarantees a
    /// single-pass backend cannot silently skip an entity kind.
    fn emit_members(&self, out: &mut String, module: &ModuleBinding, config: &Self::Config) {
        for e in &module.enums {
            self.render_enum(out, e, config);
        }
        for s in &module.structs {
            self.render_struct(out, module, s, config);
        }
        for c in &module.callbacks {
            self.render_callback(out, module, c, config);
        }
        for l in &module.listeners {
            self.render_listener(out, module, l, config);
        }
        for f in &module.functions {
            self.render_function(out, module, f, config);
        }
    }

    /// Assemble the complete output set. The driver has already built `model`
    /// (via [`BindingModel::build`] with [`prefix`](Self::prefix)) and passes
    /// the source `api` too, for the rare file (e.g. a `.pyi` stub) that needs
    /// the raw IR. Most backends render a primary source file by composing
    /// [`emit_members`](Self::emit_members) over `model.modules`, then append
    /// package manifests (`package.json`, `pyproject.toml`, `go.mod`, …) as
    /// additional [`OutputFile`]s.
    fn files(
        &self,
        api: &Api,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile>;
}

/// Build the model and write every file a backend produces.
///
/// This is the body of the [`Generator::generate`](crate::codegen::Generator)
/// impl that [`impl_generator_via_backend!`](crate::impl_generator_via_backend) generates.
pub fn run<B: LanguageBackend>(
    backend: &B,
    api: &Api,
    out_dir: &Utf8Path,
    config: &B::Config,
) -> Result<()> {
    let model = BindingModel::build(api, backend.prefix(config));
    for file in backend.files(api, &model, out_dir, config) {
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent.as_std_path())?;
        }
        std::fs::write(file.path.as_std_path(), file.contents)?;
    }
    Ok(())
}

/// Render a path for listing with `/` separators on every platform.
///
/// `Utf8Path::join` emits the platform separator, so on Windows a backend's
/// `out_dir.join("c").join("weaveffi.h")` yields `c\weaveffi.h`. The listing
/// surfaced by `--dry-run` and `weaveffi diff` (and asserted by the snapshot
/// and unit suites) must be OS-independent, so fold `\` back to `/`. A no-op
/// off Windows, where `\` is a legal filename byte we must not rewrite.
fn forward_slashes(path: Utf8PathBuf) -> String {
    let s = path.into_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// The sorted list of paths a backend would write — the body of the
/// [`Generator::output_files`](crate::codegen::Generator::output_files) impl
/// that [`impl_generator_via_backend!`](crate::impl_generator_via_backend) generates. Used by `--dry-run` and
/// `weaveffi diff`. Paths are normalised to `/` separators so the listing is
/// identical across operating systems.
pub fn output_files<B: LanguageBackend>(
    backend: &B,
    api: &Api,
    out_dir: &Utf8Path,
    config: &B::Config,
) -> Vec<String> {
    let model = BindingModel::build(api, backend.prefix(config));
    let mut paths: Vec<String> = backend
        .files(api, &model, out_dir, config)
        .into_iter()
        .map(|f| forward_slashes(f.path))
        .collect();
    paths.sort();
    paths
}

/// Re-export of `anyhow` so [`impl_generator_via_backend!`](crate::impl_generator_via_backend)
/// can name the `Generator::generate` return type in its expansion without
/// forcing every backend crate to declare a direct `anyhow` dependency it never
/// references in its own source. Not part of the public API.
#[doc(hidden)]
pub use anyhow as __anyhow;

/// Implement the object-safe [`Generator`](crate::codegen::Generator) trait for
/// a type that implements [`LanguageBackend`], delegating to the shared driver.
///
/// ```ignore
/// pub struct PythonGenerator;
/// impl weaveffi_core::backend::LanguageBackend for PythonGenerator { /* … */ }
/// weaveffi_core::impl_generator_via_backend!(PythonGenerator);
/// ```
#[macro_export]
macro_rules! impl_generator_via_backend {
    ($backend:ty) => {
        impl $crate::codegen::Generator for $backend {
            type Config = <$backend as $crate::backend::LanguageBackend>::Config;

            fn name(&self) -> &'static str {
                <$backend as $crate::backend::LanguageBackend>::name(self)
            }

            fn capabilities(&self) -> $crate::capabilities::TargetCapabilities {
                <$backend as $crate::backend::LanguageBackend>::capabilities(self)
            }

            fn allows_unsupported(&self, config: &Self::Config) -> bool {
                <$backend as $crate::backend::LanguageBackend>::allows_unsupported(self, config)
            }

            fn generate(
                &self,
                api: &::weaveffi_ir::ir::Api,
                out_dir: &::camino::Utf8Path,
                config: &Self::Config,
            ) -> $crate::backend::__anyhow::Result<()> {
                $crate::backend::run(self, api, out_dir, config)
            }

            fn output_files(
                &self,
                api: &::weaveffi_ir::ir::Api,
                out_dir: &::camino::Utf8Path,
                config: &Self::Config,
            ) -> ::std::vec::Vec<::std::string::String> {
                $crate::backend::output_files(self, api, out_dir, config)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::Generator;
    use weaveffi_ir::ir::{Function, Module, Param, TypeRef};

    #[derive(Default, Clone, serde::Serialize)]
    struct FakeConfig {
        prefix: Option<String>,
    }

    /// A trivial backend that records the canonical traversal order so we can
    /// assert the driver walks and dispatches correctly.
    struct FakeBackend;

    impl LanguageBackend for FakeBackend {
        type Config = FakeConfig;

        fn name(&self) -> &'static str {
            "fake"
        }

        fn capabilities(&self) -> TargetCapabilities {
            TargetCapabilities::full()
        }

        fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
            config.prefix.as_deref().unwrap_or("weaveffi")
        }

        fn render_enum(&self, out: &mut String, e: &EnumBinding, _c: &Self::Config) {
            out.push_str(&format!("enum {}\n", e.name));
        }

        fn render_struct(
            &self,
            out: &mut String,
            _m: &ModuleBinding,
            s: &StructBinding,
            _c: &Self::Config,
        ) {
            out.push_str(&format!("struct {}\n", s.name));
        }

        fn render_function(
            &self,
            out: &mut String,
            _m: &ModuleBinding,
            f: &FnBinding,
            _c: &Self::Config,
        ) {
            let shape = match &f.shape {
                crate::model::CallShape::Sync(_) => "sync",
                crate::model::CallShape::Async(_) => "async",
                crate::model::CallShape::Iterator(_) => "iter",
            };
            out.push_str(&format!("fn {} [{}] {}\n", f.name, shape, f.c_base));
        }

        fn files(
            &self,
            _api: &Api,
            model: &BindingModel,
            out_dir: &Utf8Path,
            config: &Self::Config,
        ) -> Vec<OutputFile> {
            let mut out = String::new();
            for m in &model.modules {
                out.push_str(&format!("module {}\n", m.path));
                self.emit_members(&mut out, m, config);
            }
            vec![OutputFile::new(out_dir.join("fake/out.txt"), out)]
        }
    }

    fn func(name: &str, returns: Option<TypeRef>, is_async: bool) -> Function {
        Function {
            name: name.into(),
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns,
            doc: None,
            r#async: is_async,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn api() -> Api {
        Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![
                    func("add", Some(TypeRef::I32), false),
                    func("fetch", Some(TypeRef::StringUtf8), true),
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        }
    }

    #[test]
    fn driver_walks_and_dispatches_in_canonical_order() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        run(&FakeBackend, &api(), out_dir, &FakeConfig::default()).unwrap();
        let body = std::fs::read_to_string(out_dir.join("fake/out.txt")).unwrap();
        assert_eq!(
            body,
            "module math\nfn add [sync] weaveffi_math_add\nfn fetch [async] weaveffi_math_fetch\n"
        );
    }

    #[test]
    fn prefix_flows_into_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let cfg = FakeConfig {
            prefix: Some("acme".into()),
        };
        run(&FakeBackend, &api(), out_dir, &cfg).unwrap();
        let body = std::fs::read_to_string(out_dir.join("fake/out.txt")).unwrap();
        assert!(
            body.contains("acme_math_add"),
            "prefix must reach symbols: {body}"
        );
        assert!(!body.contains("weaveffi_math_add"));
    }

    #[test]
    fn output_files_are_sorted_paths() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let files = output_files(&FakeBackend, &api(), out_dir, &FakeConfig::default());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("fake/out.txt"));
    }

    // Exercise the generated Generator impl.
    impl_generator_via_backend!(FakeBackend);

    #[test]
    fn generator_bridge_delegates_to_driver() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let g = FakeBackend;
        Generator::generate(&g, &api(), out_dir, &FakeConfig::default()).unwrap();
        assert!(out_dir.join("fake/out.txt").exists());
        let listed = Generator::output_files(&g, &api(), out_dir, &FakeConfig::default());
        assert_eq!(listed.len(), 1);
    }
}
