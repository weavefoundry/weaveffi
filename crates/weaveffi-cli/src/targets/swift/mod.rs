//! Swift binding generator.
//!
//! Emits a standalone SwiftPM package: the Swift wrapper module over the C
//! ABI, plus a `C{Module}` system-library target holding a copy of the C
//! header and a module map that links the native library. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as value
//! buffers. The wrapper ships a small private writer/reader pair
//! (`WvWriter`/`WvReader`, decoding in place from the library's memory) and a
//! `WvCodable` protocol: every primitive conforms, optionals, arrays, and
//! dictionaries conform generically, and each record, enum, and interface
//! conforms where it's declared, so records surface as plain Swift structs
//! and rich enums as native Swift enums with associated values, and every
//! composite type has one codec. Objects surface as `final class` wrappers
//! owning one strong reference each, callback interfaces as class-bound
//! `Sendable` protocols whose implementations cross the boundary through a
//! process-wide vtable of `@convention(c)` trampolines, and async functions
//! as `async` functions over checked continuations, with task cancellation
//! wired to the native cancel token. Before the first call the wrapper checks
//! the ABI revision and every top-level module's contract table.
//!
//! The fixed Swift sources (runtime, manifest, module map, README) live
//! under `runtime/` and are spliced with the package's names.

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
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::platform::Os;
use crate::targets::c::{header_name, render_c_header_from_model};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::targets::swift::entities::{render_swift_module_types, render_swift_namespace};
use crate::targets::swift::package::{
    render_modulemap, render_package_swift, render_packaged_readme, CSource,
};
use crate::targets::swift::runtime::{render_load_checks, render_runtime, Names};
use crate::targets::swift::types::SwiftCtx;

/// The URL a packaged manifest points its binary target at when
/// `xcframework_url` isn't set: a placeholder to replace before publishing.
pub const PLACEHOLDER_XCFRAMEWORK_URL: &str =
    "https://example.invalid/set-generators.swift.xcframework_url/{file}";

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwiftConfig {
    /// SwiftPM package, product, and module name. Defaults to the package
    /// name in PascalCase (`kvstore` becomes `Kvstore`); the C module is
    /// always this name with a `C` prefix.
    pub name: Option<String>,
    /// The minimum macOS version the manifest declares (default `11.0`, the
    /// `[build]` default deployment target).
    pub min_macos: String,
    /// The minimum iOS version the manifest declares (default `13.0`, the
    /// `[build]` default deployment target).
    pub min_ios: String,
    /// The URL `weaveffi package` points the packaged manifest's binary
    /// target at, where you upload `C{Module}.xcframework.zip`. `{version}`
    /// is replaced by the package version and `{file}` by the archive's
    /// file name. Defaults to [`PLACEHOLDER_XCFRAMEWORK_URL`].
    pub xcframework_url: Option<String>,
}

impl Default for SwiftConfig {
    fn default() -> Self {
        Self {
            name: None,
            min_macos: "11.0".into(),
            min_ios: "13.0".into(),
            xcframework_url: None,
        }
    }
}

impl SwiftConfig {
    /// The Swift module (and package and product) name for `model`.
    #[must_use]
    pub fn module_name(&self, model: &Model) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| model.identity.pascal_name())
    }

    /// The C module (and binary target) name: the module name with a `C`
    /// prefix.
    #[must_use]
    pub fn c_module_name(&self, model: &Model) -> String {
        format!("C{}", self.module_name(model))
    }

    /// The URL of the `XCFramework` archive `file` for `version`.
    #[must_use]
    pub fn xcframework_url(&self, version: &str, file: &str) -> String {
        self.xcframework_url
            .as_deref()
            .unwrap_or(PLACEHOLDER_XCFRAMEWORK_URL)
            .replace("{version}", version)
            .replace("{file}", file)
    }
}

/// The resolved names of one generated package.
struct Layout {
    /// The Swift module (and package and product) name.
    module: String,
    /// The C module name, `C{module}`.
    c_module: String,
    /// The native library base name.
    library: String,
    /// The C header file name.
    header: String,
}

impl Layout {
    fn new(model: &Model, config: &SwiftConfig) -> Self {
        let identity = &model.identity;
        let module = config.module_name(model);
        Self {
            c_module: format!("C{module}"),
            module,
            library: identity.library.clone(),
            header: header_name(model),
        }
    }

    fn names(&self) -> Names<'_> {
        Names {
            module: &self.module,
            c_module: &self.c_module,
            library: &self.library,
            header: &self.header,
        }
    }

    /// The `(path, contents)` of every source file of the package rooted at
    /// `dir`.
    fn sources(
        &self,
        model: &Model,
        dir: &Utf8Path,
        config: &SwiftConfig,
    ) -> Vec<(Utf8PathBuf, String)> {
        let c_dir = dir.join("Sources").join(&self.c_module);
        let swift_file = format!("{}.swift", self.module);
        vec![
            (
                dir.join("Package.swift"),
                render_package_swift(&self.names(), config, &CSource::SystemLibrary),
            ),
            (
                c_dir.join("module.modulemap"),
                render_modulemap(&self.names()),
            ),
            (
                c_dir.join(&self.header),
                render_c_header_from_model(model, &self.header),
            ),
            (
                dir.join("Sources").join(&self.module).join(&swift_file),
                render_swift_wrapper(self, model, &swift_file),
            ),
        ]
    }
}

/// Swift backend: emits a standalone SwiftPM package wrapping the C ABI.
pub struct SwiftGenerator;

impl LanguageBackend for SwiftGenerator {
    type Config = SwiftConfig;

    fn name(&self) -> &'static str {
        "swift"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        Layout::new(model, config)
            .sources(model, &out_dir.join("swift"), config)
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    /// The SwiftPM package at `swift/{Module}/` whose binary target is the
    /// `C{Module}.xcframework.zip` archive `weaveffi package` assembles from
    /// the Apple static libraries (at the configured `xcframework_url`, with
    /// its checksum). Without an archive (no Apple platform built, or not on
    /// macOS) there is nothing to package.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let Some(archive) = ctx.xcframework else {
            return Some(Vec::new());
        };
        let layout = Layout::new(model, config);
        let names = layout.names();
        let url = config.xcframework_url(&model.identity.version, &archive.file_name);
        let swift_file = format!("{}.swift", layout.module);
        let slices = ctx
            .binaries
            .binaries
            .iter()
            .filter(|nb| matches!(nb.platform.os(), Os::MacOs | Os::Ios) && nb.staticlib.is_some())
            .map(|nb| format!("`{}`", nb.platform.id()))
            .collect::<Vec<_>>()
            .join(", ");
        let files = vec![
            PackagedFile::text(
                "Package.swift",
                render_package_swift(
                    &names,
                    config,
                    &CSource::Remote {
                        url: &url,
                        checksum: &archive.checksum,
                    },
                ),
            ),
            PackagedFile::text(
                format!("Sources/{}/{swift_file}", layout.module),
                render_swift_wrapper(&layout, model, &swift_file),
            ),
            PackagedFile::text(
                "README.md",
                render_packaged_readme(&names, &slices, &url, &archive.checksum),
            ),
        ];
        Some(vec![Artifact::directory(
            format!("swift/{}", layout.module),
            files,
        )])
    }
}

/// Render the complete Swift wrapper file: prelude and imports, the private
/// runtime, the load-time checks, every module's file-scope types, and one
/// namespace `enum` per top-level module.
fn render_swift_wrapper(layout: &Layout, model: &Model, filename: &str) -> String {
    let ctx = SwiftCtx::new(model, &layout.module);
    let mut w = CodeWriter::four_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line(format!("import {}", layout.c_module));
    w.line("import Foundation");
    w.blank();
    w.raw(render_runtime(model, &layout.names(), &ctx.runtime_error));
    w.blank();
    render_load_checks(&mut w, model);
    w.line("// MARK: - API");
    w.blank();
    for mb in &model.modules {
        render_swift_module_types(&mut w, mb, &ctx);
    }
    for root in model.roots() {
        render_swift_namespace(&mut w, model, root, &ctx);
        w.blank();
    }
    w.raw(render_trailer(CommentStyle::DoubleSlash, filename));
    w.finish()
}
