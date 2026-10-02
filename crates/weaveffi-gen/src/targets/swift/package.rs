//! SwiftPM packaging surfaces: `Package.swift`, the system-library module
//! map, and the packaged-artifact README, framed with the generated-file
//! prelude and trailer.

use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use crate::targets::swift::runtime::{render_module_map, render_package, render_readme, Names};

/// The SwiftPM tools version the manifest declares (Swift 5 language mode).
const TOOLS_VERSION: &str = "5.9";

/// Render `Package.swift`. The manifest picks the C module's source at
/// resolution time: a prebuilt `C{Module}.xcframework` next to it when one
/// exists, otherwise the bundled system-library target.
pub(crate) fn render_package_swift(names: &Names, input_basename: &str) -> String {
    // `swift-tools-version` must be the very first line of the manifest, so
    // the generated-file prelude follows it.
    format!(
        "// swift-tools-version:{TOOLS_VERSION}\n{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        render_package(names),
        render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
    )
}

/// Render the module map of the `C{Module}` system-library target, which
/// sits next to its copy of the C header.
pub(crate) fn render_modulemap(names: &Names, input_basename: &str) -> String {
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        render_module_map(names),
        render_trailer(CommentStyle::DoubleSlash, "module.modulemap"),
    )
}

/// README for a packaged Swift artifact: linking the bundled libraries and
/// assembling the optional `XCFramework`, the one step that needs Apple
/// tooling.
pub(crate) fn render_packaged_readme(
    names: &Names,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let platforms = ctx
        .binaries
        .platforms()
        .filter(|p| p.is_desktop())
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::Xml, input_basename),
        render_readme(names, &platforms),
        render_trailer(CommentStyle::Xml, "README.md"),
    )
}
