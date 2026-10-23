//! Swift binding generator.
//!
//! Emits a standalone SwiftPM package: the Swift wrapper module over the C
//! ABI (the API file plus a fixed runtime file), and a `C{Module}`
//! system-library target holding a copy of the C header and a module map
//! that links the native library.
//!
//! Records, rich enums, and every optional, list, and map that isn't an
//! optional scalar or a numeric list cross the C ABI as value buffers. The
//! runtime ships a small private writer/reader pair (`WvWriter`/`WvReader`,
//! decoding in place from the library's memory) and a `WvCodable` protocol:
//! every primitive conforms, optionals, arrays, and dictionaries conform
//! generically, and each record, enum, and interface conforms where it's
//! declared, so records surface as plain `Hashable` Swift structs and rich
//! enums as native Swift enums with associated values. Optional scalars
//! (`T?`) cross directly as a presence flag and a value, and numeric lists
//! (`[T]`) as the array's own storage. Objects surface as `final class`
//! wrappers owning one strong reference each, `Hashable` by identity;
//! callback interfaces as class-bound `Sendable` protocols whose
//! implementations cross through a process-wide vtable of `@convention(c)`
//! trampolines; iterators as the runtime's generic `NativeSequence`; and
//! async functions as `async` functions over checked continuations, with task
//! cancellation wired to the native cancel token. Error domains are open
//! `Error` enums. `{Module}Library.check()` reports a library that doesn't
//! match the bindings as a thrown error; a call made while it doesn't
//! match stops the process.
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
mod xcframework;

use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::platform::Os;
use crate::targets::c::{header_name, render_c_header_from_model};
use crate::targets::{Linkage, Target};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::codegen::errors;
use crate::targets::swift::entities::{
    render_swift_errors, render_swift_module_types, render_swift_namespace,
};
use crate::targets::swift::package::{
    render_modulemap, render_package_swift, render_packaged_readme, CSource,
};
use crate::targets::swift::runtime::{render_contract_checks, render_runtime, Names, RUNTIME_FILE};
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
        let swift_dir = dir.join("Sources").join(&self.module);
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
                swift_dir.join(&swift_file),
                render_swift_wrapper(self, model, &swift_file),
            ),
            (swift_dir.join(RUNTIME_FILE), self.runtime(model)),
        ]
    }

    /// The runtime source file, framed with the prelude and trailer.
    fn runtime(&self, model: &Model) -> String {
        format!(
            "{}{}\n{}",
            render_prelude(CommentStyle::DoubleSlash),
            render_runtime(model, &self.names()),
            render_trailer(CommentStyle::DoubleSlash, RUNTIME_FILE),
        )
    }
}

/// Swift backend: emits a standalone SwiftPM package wrapping the C ABI.
pub struct SwiftGenerator {
    config: SwiftConfig,
}

impl From<SwiftConfig> for SwiftGenerator {
    fn from(config: SwiftConfig) -> Self {
        Self { config }
    }
}

impl Target for SwiftGenerator {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let config = &self.config;
        Layout::new(model, config)
            .sources(model, &Utf8PathBuf::new(), config)
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    fn linkage(&self) -> Linkage {
        Linkage::Link
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &["Package.swift", "module.modulemap", RUNTIME_FILE]
    }

    /// The SwiftPM package at `swift/{Module}/` whose binary target is the
    /// `C{Module}.xcframework.zip` archive assembled here from the Apple
    /// static libraries (at the configured `xcframework_url`, with its
    /// checksum), written next to it with a `.sha256` file. Without an
    /// archive (no Apple platform built, or no Xcode) there is nothing to
    /// package.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
        let Some(archive) = xcframework::assemble(model, &config.c_module_name(model), ctx)? else {
            return Ok(Vec::new());
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
                format!("Sources/{}/{RUNTIME_FILE}", layout.module),
                layout.runtime(model),
            ),
            PackagedFile::text(
                "README.md",
                render_packaged_readme(&names, &slices, &url, &archive.checksum),
            ),
        ];
        let checksum_file = format!("{}  {}\n", archive.checksum, archive.file_name);
        Ok(vec![
            Artifact::single_file(
                format!("swift/{}.sha256", archive.file_name),
                checksum_file.into_bytes(),
            ),
            Artifact::single_file(format!("swift/{}", archive.file_name), archive.bytes),
            Artifact::directory(format!("swift/{}", layout.module), files),
        ])
    }
}

/// Render the Swift API file: prelude and imports, the contract rows the
/// load check compares, every error domain, every module's file-scope
/// types, and one namespace `enum` per top-level module.
fn render_swift_wrapper(layout: &Layout, model: &Model, filename: &str) -> String {
    let ctx = SwiftCtx::new(model, &layout.module);
    let mut w = CodeWriter::four_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line(format!("import {}", layout.c_module));
    w.line("import Foundation");
    w.blank();
    render_contract_checks(&mut w, model, &ctx.library_type);
    w.line("// MARK: - API");
    w.blank();
    render_swift_errors(&mut w, &errors::tables(model, "Error"), &ctx);
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
