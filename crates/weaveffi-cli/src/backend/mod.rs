//! The language-backend framework.
//!
//! [`LanguageBackend`] is the one trait a target implements: a stable name,
//! a pure [`files`](LanguageBackend::files) that renders the complete output
//! set from the [`Model`], and an optional
//! [`package`](LanguageBackend::package) for `weaveffi package`.
//! [`ConfiguredBackend`](crate::codegen::ConfiguredBackend) exposes the
//! backend to the orchestrator through the object-safe
//! [`Target`](crate::codegen::Target) trait. A backend owns *only*
//! language-specific rendering: type mapping, marshalling, file layout, and
//! the exact text of each declaration. Model construction, erasure, and every
//! write live elsewhere, once.
//!
//! [`Model`]: weaveffi_model::model::Model

use camino::{Utf8Path, Utf8PathBuf};

use crate::package::{Artifact, PackageContext};
use weaveffi_model::model::Model;

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

/// An idiomatic language backend over the shared [`Model`].
///
/// The required methods are [`name`](Self::name) and [`files`](Self::files),
/// which assembles the complete output set; wrap the type in
/// [`ConfiguredBackend`](crate::codegen::ConfiguredBackend) to hand it to the
/// orchestrator. Rendering is pure: a backend returns [`OutputFile`]s and the
/// orchestrator does the I/O. Each backend walks the model in whatever order
/// its language needs (C and C++ order declarations by dependency, Swift
/// splits types from the namespaced module body, Kotlin, Node.js, and Wasm
/// render parallel files) and emits its own doc comments, sharing
/// [`emit_doc`](crate::codegen::common::emit_doc) for the common line and
/// block flavors.
pub trait LanguageBackend: Send + Sync {
    /// Per-target, fully typed configuration (the `[generators.<target>]`
    /// table).
    type Config: Default + Clone + Send + Sync;

    /// Stable short name (`"swift"`, `"python"`, ...): the `--target` token
    /// and the generation-record file basename.
    fn name(&self) -> &'static str;

    /// Assemble the complete output set from the validated `model`, which
    /// carries the library's identity alongside every symbol and signature.
    /// Most backends render a primary source file from `model.modules`, then
    /// append package manifests (`package.json`, `pyproject.toml`, `go.mod`,
    /// ...) as additional [`OutputFile`]s.
    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile>;

    /// Assemble the installable artifacts that bundle the per-platform builds
    /// in `ctx.binaries`, returning `None` when this target has no packaging.
    ///
    /// This is the `weaveffi package` analogue of [`files`](Self::files): it
    /// returns [`Artifact`]s (wheels, npm tarballs, gems, directory trees)
    /// with paths relative to the dist directory, and the
    /// [`write_artifact`](crate::package::write_artifact) driver does the
    /// I/O. A target skips platforms its ecosystem has no slot for and
    /// returns an empty list when none of `ctx.binaries` fits. The default
    /// returns `None`.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let _ = (model, ctx, config);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ir::{Api, Function, Module, Param, TypeRef};
    use weaveffi_model::pkg::Identity;
    use weaveffi_model::ty::Prim;

    #[derive(Default, Clone)]
    struct FakeConfig;

    /// A trivial backend that lists each function's C symbol, so we can
    /// assert the model carries the identity's prefix.
    struct FakeBackend;

    impl LanguageBackend for FakeBackend {
        type Config = FakeConfig;

        fn name(&self) -> &'static str {
            "fake"
        }

        fn files(
            &self,
            model: &Model,
            out_dir: &Utf8Path,
            _config: &Self::Config,
        ) -> Vec<OutputFile> {
            let mut out = String::new();
            for m in &model.modules {
                out.push_str(&format!("module {}\n", m.path));
                for f in &m.functions {
                    out.push_str(&format!("fn {} {}\n", f.name, f.c_base));
                }
            }
            vec![OutputFile::new(out_dir.join("fake/out.txt"), out)]
        }
    }

    fn model(name: &str) -> Model {
        let api = Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "math".into(),
                doc: None,
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![Param {
                        name: "x".into(),
                        ty: TypeRef::Prim(Prim::I32),
                        doc: None,
                    }],
                    returns: Some(TypeRef::Prim(Prim::I32)),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                }],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callback_interfaces: vec![],
                errors: None,
                modules: vec![],
            }],
        };
        weaveffi_model::validate::validate(&api, &Identity::named(name), None).unwrap()
    }

    #[test]
    fn backends_render_with_the_identity_prefix() {
        use crate::codegen::{ConfiguredBackend, Target};
        let out_dir = Utf8Path::new("out");
        let render = |n: &str| {
            let files = ConfiguredBackend::new(FakeBackend, FakeConfig).render(&model(n), out_dir);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, out_dir.join("fake/out.txt"));
            files[0].contents.clone()
        };
        assert_eq!(
            render("weaveffi"),
            "module math\nfn add weaveffi_math_add\n"
        );
        assert_eq!(render("acme"), "module math\nfn add acme_math_add\n");
    }
}
