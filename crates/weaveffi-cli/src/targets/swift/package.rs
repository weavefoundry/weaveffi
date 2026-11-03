//! SwiftPM packaging surfaces: `Package.swift` (generated and packaged), the
//! system-library module map, and the packaged README, framed with the
//! generated-file prelude and trailer.

use crate::utils::{render_prelude, render_trailer, CommentStyle};

use crate::targets::swift::runtime::{render_module_map, render_package, render_readme, Names};
use crate::targets::swift::SwiftConfig;

/// The SwiftPM tools version the manifest declares (Swift 5 language mode).
const TOOLS_VERSION: &str = "5.9";

/// Where a manifest's C module comes from when no `XCFramework` sits next to
/// it.
pub(crate) enum CSource<'a> {
    /// The generated `Sources/C{Module}` system-library target, linking the
    /// library from the linker search path.
    SystemLibrary,
    /// A published `XCFramework` archive.
    Remote {
        /// The archive's URL.
        url: &'a str,
        /// The archive's SHA-256 checksum.
        checksum: &'a str,
    },
}

/// The manifest's `platforms:` list from the configured minimum versions.
pub(crate) fn platforms(config: &SwiftConfig) -> String {
    format!(
        ".macOS(\"{}\"), .iOS(\"{}\")",
        config.min_macos, config.min_ios
    )
}

/// Render `Package.swift`. The manifest picks the C module's source at
/// resolution time: a prebuilt `C{Module}.xcframework` next to it when one
/// exists, otherwise `source`.
pub(crate) fn render_package_swift(
    names: &Names,
    config: &SwiftConfig,
    source: &CSource,
) -> String {
    let c = names.c_module;
    let (comment, fallback) = match source {
        CSource::SystemLibrary => (
            format!(
                "// The C module `{c}` comes from one of two places. Next to this\n\
                 // manifest, a prebuilt `{c}.xcframework` (from `weaveffi package`) wins.\n\
                 // Otherwise `Sources/{c}` declares the generated header as a system\n\
                 // library that links `{}` from the linker search path, e.g.\n\
                 // `swift build -Xlinker -L/path/to/lib`.",
                names.library
            ),
            format!(".systemLibrary(name: \"{c}\")"),
        ),
        CSource::Remote { url, checksum } => (
            format!(
                "// The C module `{c}` is the prebuilt `{c}.xcframework`, published as\n\
                 // `{c}.xcframework.zip`. A copy unzipped next to this manifest wins,\n\
                 // for local development."
            ),
            format!(
                ".binaryTarget(\n        name: \"{c}\",\n        url: \"{url}\",\n        \
                 checksum: \"{checksum}\"\n    )"
            ),
        ),
    };
    // `swift-tools-version` must be the very first line of the manifest, so
    // the generated-file prelude follows it.
    format!(
        "// swift-tools-version:{TOOLS_VERSION}\n{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash),
        render_package(names, &comment, &fallback, &platforms(config)),
        render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
    )
}

/// Render the module map of the `C{Module}` system-library target, which
/// sits next to its copy of the C header.
pub(crate) fn render_modulemap(names: &Names) -> String {
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash),
        render_module_map(names),
        render_trailer(CommentStyle::DoubleSlash, "module.modulemap"),
    )
}

/// README for the packaged Swift package: the `XCFramework` slices and how
/// to publish the archive its binary target points at.
pub(crate) fn render_packaged_readme(
    names: &Names,
    slices: &str,
    url: &str,
    checksum: &str,
) -> String {
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::Xml),
        render_readme(names, slices, url, checksum),
        render_trailer(CommentStyle::Xml, "README.md"),
    )
}
