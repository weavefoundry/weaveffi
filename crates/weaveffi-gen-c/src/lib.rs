//! C header generator for WeaveFFI.
//!
//! Emits a single `{prefix}.h` describing the stable C ABI surface of a
//! [`ResolvedApi`], plus a companion
//! `{prefix}.c` placeholder for future convenience
//! wrappers. This is the canonical backend: the header it emits *is* the C ABI
//! every other language binds to.
//!
//! Like every WeaveFFI backend it renders from the shared
//! [`weaveffi_core::model::BindingModel`], so symbol names and parameter
//! lowering are computed once and shared, never re-derived here.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod header;
mod idents;
mod package;
#[cfg(test)]
mod tests;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::resolved::ResolvedApi;

pub use header::{render_c_header, render_c_header_from_model};

use header::render_c_convenience_c;
use package::{render_packaged_cmake, render_packaged_readme};

/// Per-target configuration for [`CGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CConfig {
    /// Prefix applied to every emitted C symbol (default `"weaveffi"`).
    /// Renames produce both `prefix_*` user symbols and
    /// `#define prefix_runtime weaveffi_runtime` aliases for the ABI helpers.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with (e.g. `weaveffi.yml`).
    /// Embedded in the prelude header of every generated file. Populated
    /// by the CLI; not user-configurable via the `[c]` config section.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl CConfig {
    /// Returns the configured symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// C backend: emits the canonical `{prefix}.h` header describing the stable
/// C ABI surface, plus a `{prefix}.c` companion for future wrappers.
pub struct CGenerator;

impl LanguageBackend for CGenerator {
    type Config = CConfig;

    fn name(&self) -> &'static str {
        "c"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        _api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("c");
        let header_name = format!("{prefix}.h");
        let source_name = format!("{prefix}.c");
        vec![
            OutputFile::new(
                dir.join(&header_name),
                render_c_header_from_model(model, input_basename, &header_name),
            ),
            OutputFile::new(
                dir.join(&source_name),
                render_c_convenience_c(prefix, input_basename, &source_name),
            ),
        ]
    }

    fn package(
        &self,
        _api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("c");
        let header_name = format!("{prefix}.h");
        let lib = &ctx.binaries.lib_name;

        let mut files = vec![
            PackagedFile::text(
                dir.join("include").join(&header_name),
                render_c_header_from_model(model, input_basename, &header_name),
            ),
            PackagedFile::text(
                dir.join("CMakeLists.txt"),
                render_packaged_cmake(lib, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(lib, &header_name, ctx, input_basename),
            ),
        ];
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(CGenerator);
