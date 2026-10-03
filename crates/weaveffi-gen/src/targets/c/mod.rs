//! C header generator for WeaveFFI.
//!
//! Emits `{library}.h`, describing the stable C ABI surface of a
//! [`ResolvedApi`]. This is the canonical backend: the header it emits *is* the
//! C ABI every other language binds to, and a C or C++ producer implementing
//! an IDL by hand implements exactly the functions it declares.
//!
//! When the API passes any value buffers and [`CConfig::buffer_helpers`] is
//! on (the default), it also emits `{library}_buffer.h`: C structs for the
//! API's records, rich enums, lists, and maps, with `static inline` codecs
//! that encode and decode them (see [`render_buffer_header`]).
//!
//! Like every WeaveFFI backend it renders from the shared
//! [`weaveffi_model::model::BindingModel`], so symbol names and parameter
//! lowering are computed once and shared, never re-derived here.

mod buffer;
mod header;
mod package;
use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

pub use buffer::{buffer_header_name, render_buffer_header};
pub use header::{render_c_header, render_c_header_from_model};

use package::{render_packaged_cmake, render_packaged_readme};

/// Per-target configuration for [`CGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CConfig {
    /// Basename of the IDL the CLI was invoked with (e.g. `kvstore.yml`).
    /// Embedded in the prelude header of every generated file. Populated
    /// by the CLI; not user-configurable via the `[c]` config section.
    #[serde(skip)]
    pub input_basename: Option<String>,
    /// Emit `{library}_buffer.h`, the value-buffer helper header, next to
    /// `{library}.h` when the API passes any value buffers. On by default.
    pub buffer_helpers: bool,
}

impl Default for CConfig {
    fn default() -> Self {
        Self {
            input_basename: None,
            buffer_helpers: true,
        }
    }
}

impl CConfig {
    /// The IDL basename for generated-file preludes.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// The header file name for an API: `{library}.h`.
#[must_use]
pub fn header_name(api: &ResolvedApi) -> String {
    format!("{}.h", api.identity().library)
}

/// The helper header's name and contents, when the configuration asks for it
/// and the API has value buffers to describe.
fn buffer_file(
    api: &ResolvedApi,
    model: &BindingModel,
    config: &CConfig,
) -> Option<(String, String)> {
    if !config.buffer_helpers {
        return None;
    }
    let name = buffer_header_name(&api.identity().library);
    render_buffer_header(model, config.input_basename(), &header_name(api), &name)
        .map(|contents| (name, contents))
}

/// The C header backend.
pub struct CGenerator;

impl LanguageBackend for CGenerator {
    type Config = CConfig;

    fn name(&self) -> &'static str {
        "c"
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
        let header = header_name(api);
        let mut files = vec![OutputFile::new(
            out_dir.join("c").join(&header),
            render_c_header_from_model(model, config.input_basename(), &header),
        )];
        if let Some((name, contents)) = buffer_file(api, model, config) {
            files.push(OutputFile::new(out_dir.join("c").join(name), contents));
        }
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
        let input_basename = config.input_basename();
        let dir = out_dir.join("c");
        let header_name = header_name(api);
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
        if let Some((name, contents)) = buffer_file(api, model, config) {
            files.push(PackagedFile::text(dir.join("include").join(name), contents));
        }
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
