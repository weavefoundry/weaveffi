//! C header generator for WeaveFFI.
//!
//! Emits `{library}.h`, describing the stable C ABI surface of a [`Model`].
//! This is the canonical backend: the header it emits *is* the C ABI every
//! other language binds to, and a C or C++ producer implementing
//! an IDL by hand implements exactly the functions it declares.
//!
//! When the API passes any value buffers and [`CConfig::buffer_helpers`] is
//! on (the default), it also emits `{library}_buffer.h`: C structs for the
//! API's records, rich enums, lists, and maps, with `static inline` codecs
//! that encode and decode them (see [`render_buffer_header`]).
//!
//! Like every WeaveFFI backend it renders from the shared [`Model`], so
//! symbol names and parameter lowering are computed once and shared, never
//! re-derived here.

mod buffer;
mod header;
mod package;
use crate::backend::{LanguageBackend, OutputFile};
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

pub use buffer::{buffer_header_name, render_buffer_header};
pub use header::render_c_header_from_model;

use package::{render_packaged_cmake, render_packaged_readme};

/// Per-target configuration for [`CGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CConfig {
    /// Emit `{library}_buffer.h`, the value-buffer helper header, next to
    /// `{library}.h` when the API passes any value buffers. On by default.
    pub buffer_helpers: bool,
}

impl Default for CConfig {
    fn default() -> Self {
        Self {
            buffer_helpers: true,
        }
    }
}

/// The header file name for an API: `{library}.h`.
#[must_use]
pub fn header_name(model: &Model) -> String {
    format!("{}.h", model.identity.library)
}

/// The helper header's name and contents, when the configuration asks for it
/// and the API has value buffers to describe.
fn buffer_file(model: &Model, config: &CConfig) -> Option<(String, String)> {
    if !config.buffer_helpers {
        return None;
    }
    let name = buffer_header_name(&model.identity.library);
    render_buffer_header(model, &header_name(model), &name).map(|contents| (name, contents))
}

/// The C header backend.
pub struct CGenerator;

impl LanguageBackend for CGenerator {
    type Config = CConfig;

    fn name(&self) -> &'static str {
        "c"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let header = header_name(model);
        let mut files = vec![OutputFile::new(
            out_dir.join("c").join(&header),
            render_c_header_from_model(model, &header),
        )];
        if let Some((name, contents)) = buffer_file(model, config) {
            files.push(OutputFile::new(out_dir.join("c").join(name), contents));
        }
        files
    }

    /// One `{library}-{version}-c.tar.gz` holding the headers under
    /// `include/`, every platform's library under `lib/<platform>/`, and a
    /// `CMakeLists.txt` exposing the host's library as an imported target.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let header_name = header_name(model);
        let lib = &ctx.binaries.lib_name;
        let mut files = vec![
            PackagedFile::text(
                format!("include/{header_name}"),
                render_c_header_from_model(model, &header_name),
            ),
            PackagedFile::text("CMakeLists.txt", render_packaged_cmake(lib)),
            PackagedFile::text("README.md", render_packaged_readme(lib, &header_name, ctx)),
        ];
        if let Some((name, contents)) = buffer_file(model, config) {
            files.push(PackagedFile::text(format!("include/{name}"), contents));
        }
        files.extend(per_platform_libraries(ctx.binaries, "lib", |_| true));
        let stem = format!("{lib}-{}", model.identity.version);
        Some(vec![Artifact::tar_gz(
            format!("c/{stem}-c.tar.gz"),
            stem,
            files,
        )])
    }
}
