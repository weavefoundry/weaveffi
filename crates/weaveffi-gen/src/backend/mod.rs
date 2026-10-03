//! The language-backend framework.
//!
//! Every idiomatic WeaveFFI generator does the same three things: it walks the
//! [`BindingModel`] in a fixed order (errors, enums, structs, callback
//! interfaces, interfaces, functions), dispatches each function on its
//! [`CallShape`], and writes a primary source file plus a handful of package
//! manifests.
//!
//! [`LanguageBackend`] is the one trait a target implements. It captures the
//! common structure as hooks; [`ConfiguredBackend`](crate::codegen::ConfiguredBackend)
//! builds the model and exposes the backend to the orchestrator through the
//! object-safe [`Target`](crate::codegen::Target) trait. A backend owns *only* language-specific rendering: type mapping,
//! marshalling, and the exact text of each declaration. The traversal order,
//! the call-shape dispatch, the model construction, and the erasure all live
//! here, once.
//!
//! [`BindingModel`]: weaveffi_model::model::BindingModel
//! [`CallShape`]: weaveffi_model::model::CallShape

use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;

use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use weaveffi_model::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    ModuleBinding, StructBinding,
};
use weaveffi_model::resolved::ResolvedApi;

/// A single generated file: its full path (under the output directory) and the
/// rendered contents. Backends return these from [`LanguageBackend::files`];
/// the driver creates parent directories and writes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Full path to write, under (or anchored at) the output directory.
    pub path: Utf8PathBuf,
    /// The rendered file contents.
    pub contents: String,
}

impl OutputFile {
    /// Pair a destination path with its rendered contents.
    ///
    /// The path is normalized to `/` separators, which every platform's file
    /// APIs accept, so listings, cache records, and tests see the same path
    /// on Windows as elsewhere.
    pub fn new(path: impl Into<Utf8PathBuf>, contents: impl Into<String>) -> Self {
        let path: Utf8PathBuf = path.into();
        let path = if path.as_str().contains('\\') {
            Utf8PathBuf::from(path.as_str().replace('\\', "/"))
        } else {
            path
        };
        Self {
            path,
            contents: contents.into(),
        }
    }
}

/// An idiomatic language backend over the shared [`BindingModel`].
///
/// The required methods are [`name`](Self::name),
/// [`capabilities`](Self::capabilities), and [`files`](Self::files), which
/// assembles the complete output set; wrap the type in
/// [`ConfiguredBackend`](crate::codegen::ConfiguredBackend) to hand it to the
/// orchestrator. That alone gives every backend the shared driver and the
/// [`OutputFile`] model (rendering is pure; the orchestrator does the I/O).
///
/// Backends whose primary file is a straightforward per-module walk (Python,
/// Ruby, Go, Dart, .NET) override the per-entity hooks (`render_enum`,
/// `render_struct`, `render_callback_interface`, `render_interface`,
/// `render_function`) and call the provided
/// [`emit_members`](Self::emit_members) from inside their module scoping; that
/// is what removes the hand-rolled walk + call-shape dispatch each generator
/// used to carry. Backends whose output is not one linear pass leave the hooks
/// at their no-op defaults and build their layout directly in
/// [`files`](Self::files): C and C++ order declarations by dependency, Swift
/// splits types from the namespaced module body, and Kotlin, Node, and Wasm
/// each render parallel files (Kotlin + JNI C, addon C + JS, JS + `.d.ts`)
/// that would each need their own walk anyway.
///
/// Each hook renders into a `String` (matching how generators accumulate
/// output) and is responsible for emitting its own doc comments; doc-comment
/// shape varies too much between targets (docstrings, `///`, KDoc, `<summary>`)
/// to centralise here, but every backend shares
/// [`emit_doc`](crate::codegen::common::emit_doc) for the line/block flavours.
pub trait LanguageBackend: Send + Sync {
    /// Per-target, fully-typed configuration. Must round-trip through
    /// `serde_json` so the orchestrator can hash it as part of the cache key.
    type Config: Serialize + Default + Clone + Send + Sync;

    /// Stable short name (`"swift"`, `"python"`, ...): the `--target` token
    /// and the cache file basename.
    fn name(&self) -> &'static str;

    /// The gated IDL features this backend implements (async functions,
    /// callback interfaces, iterators) under `config`. Required: declaring
    /// capabilities explicitly is what lets the orchestrator fail loudly
    /// instead of a backend silently skipping a feature it never implemented.
    /// The config is passed because some backends have modes with different
    /// ceilings (the Wasm backend's Emscripten mode, for example).
    fn capabilities(&self, config: &Self::Config) -> TargetCapabilities;

    /// Whether the user explicitly opted in to generating this target even
    /// though the API uses features the target does not support (via an
    /// `allow_unsupported = true` flag in the target's config). When `true`
    /// the orchestrator downgrades the capability failure to a loud warning
    /// and the backend must emit an explicit unsupported surface (throwing
    /// stubs, documentation) rather than silently omitting the feature.
    /// Backends with partial capabilities override this to read their
    /// `allow_unsupported` config flag; full-capability backends keep the
    /// `false` default.
    fn allows_unsupported(&self, config: &Self::Config) -> bool {
        let _ = config;
        false
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

    /// Render the typed error surface for a module that *declares* an error
    /// domain: the target's error enum/class hierarchy mapping each
    /// [`ErrorBinding`] code to a case the consumer can match on. Override when
    /// using [`emit_members`](Self::emit_members); inheriting modules reference
    /// the ancestor's type, so this hook only fires where
    /// [`ModuleBinding::declares_error`] is true.
    fn render_error(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        e: &ErrorBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, e, config);
    }

    /// Render one callback interface: the protocol, abstract class, or
    /// interface type the consumer implements, plus the vtable trampolines
    /// that adapt an implementation to the C ABI. Override when using
    /// [`emit_members`](Self::emit_members).
    fn render_callback_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, cb, config);
    }

    /// Render one interface: the wrapper class with its constructors, methods,
    /// statics, and reference-count wiring. Override when using
    /// [`emit_members`](Self::emit_members).
    fn render_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        i: &InterfaceBinding,
        config: &Self::Config,
    ) {
        let _ = (out, module, i, config);
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

    /// Emit every member of `module` in canonical order (error domain, enums,
    /// structs, callback interfaces, interfaces, functions). Callback
    /// interfaces precede interfaces because interface members take them as
    /// parameters. Backends call this from within their own module scoping;
    /// overriding the per-entity hooks is what guarantees a single-pass
    /// backend cannot silently skip an entity kind.
    fn emit_members(&self, out: &mut String, module: &ModuleBinding, config: &Self::Config) {
        if let Some(e) = module.error.as_ref().filter(|e| e.declared_here) {
            self.render_error(out, module, e, config);
        }
        for e in &module.enums {
            self.render_enum(out, e, config);
        }
        for s in &module.structs {
            self.render_struct(out, module, s, config);
        }
        for cb in &module.callback_interfaces {
            self.render_callback_interface(out, module, cb, config);
        }
        for i in &module.interfaces {
            self.render_interface(out, module, i, config);
        }
        for f in &module.functions {
            self.render_function(out, module, f, config);
        }
    }

    /// Assemble the complete output set. The driver has already built `model`
    /// (via [`BindingModel::build`], with the API identity's prefix) and passes
    /// the resolved `api` too, for the rare file (e.g. a `.pyi` stub) that
    /// needs the full resolved tree. Most backends render a primary source
    /// file by composing [`emit_members`](Self::emit_members) over
    /// `model.modules`, then append package manifests (`package.json`,
    /// `pyproject.toml`, `go.mod`, …) as additional [`OutputFile`]s.
    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile>;

    /// Assemble a distributable package that bundles a prebuilt native library
    /// for each platform in `ctx.binaries`, returning `None` when this target
    /// does not support packaging yet.
    ///
    /// This is the `weaveffi package` analogue of [`files`](Self::files): it
    /// returns [`PackagedFile`]s (rendered manifests, loaders, and binding
    /// source as [`FileContent::Text`](crate::package::FileContent::Text), plus
    /// the bundled libraries as
    /// [`FileContent::Copy`](crate::package::FileContent::Copy)) anchored under
    /// `out_dir`, and the [`write_package`](crate::package::write_package)
    /// driver does the I/O. Override this to emit the ecosystem's idiomatic
    /// per-platform layout (npm `optionalDependencies`, a NuGet `runtimes/`
    /// tree, platform-tagged Python wheels, …). The default returns `None`.
    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let _ = (api, model, ctx, out_dir, config);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ir::{Api, Function, Module, Param, TypeRef};
    use weaveffi_model::model::CallShape;

    #[derive(Default, Clone, serde::Serialize)]
    struct FakeConfig;

    /// A trivial backend that records the canonical traversal order so we can
    /// assert the driver walks and dispatches correctly.
    struct FakeBackend;

    impl LanguageBackend for FakeBackend {
        type Config = FakeConfig;

        fn name(&self) -> &'static str {
            "fake"
        }

        fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
            TargetCapabilities::full()
        }

        fn render_function(
            &self,
            out: &mut String,
            _m: &ModuleBinding,
            f: &FnBinding,
            _c: &Self::Config,
        ) {
            let shape = match &f.shape {
                CallShape::Sync(_) => "sync",
                CallShape::Async(_) => "async",
                CallShape::Iterator(_) => "iter",
            };
            out.push_str(&format!("fn {} [{}] {}\n", f.name, shape, f.c_base));
        }

        fn files(
            &self,
            _api: &ResolvedApi,
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
                doc: None,
            }],
            returns,
            doc: None,
            throws: false,
            r#async: is_async,
            cancellable: false,
            deprecated: None,
        }
    }

    fn api() -> ResolvedApi {
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "math".into(),
                doc: None,
                functions: vec![
                    func("add", Some(TypeRef::I32), false),
                    func("fetch", Some(TypeRef::StringUtf8), true),
                    func(
                        "scan",
                        Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                        false,
                    ),
                ],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callback_interfaces: vec![],
                errors: None,
                modules: vec![],
            }],
        })
    }

    #[test]
    fn driver_walks_dispatches_and_honors_the_identity_prefix() {
        use crate::codegen::{ConfiguredBackend, Target};
        let out_dir = Utf8Path::new("out");
        let render = |n: &str| {
            let api = api().with_identity(weaveffi_model::pkg::Identity::named(n));
            let files = ConfiguredBackend::new(FakeBackend, FakeConfig).render(&api, out_dir);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, out_dir.join("fake/out.txt"));
            files[0].contents.clone()
        };
        assert_eq!(
            render("weaveffi"),
            "module math\nfn add [sync] weaveffi_math_add\nfn fetch [async] weaveffi_math_fetch\nfn scan [iter] weaveffi_math_scan\n"
        );
        let body = render("acme");
        assert!(body.contains("acme_math_add"), "{body}");
        assert!(!body.contains("weaveffi_math_add"));
    }
}
