//! SwiftPM packaging surfaces: `Package.swift`, the system-library module
//! map, and the packaged-artifact README.
//!
//! `Package.swift` is Swift source, so every interpolated user string (the
//! package name and the target names derived from it) routes through
//! [`swift_str`] so quotes or backslashes in a configured module name can't
//! corrupt the manifest.

use heck::ToUpperCamelCase;
use weaveffi_core::package::PackageContext;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::types::swift_str;
use crate::SwiftConfig;

/// The SwiftPM package/module name: an explicit `[swift] module_name` wins;
/// otherwise the `[package] name` from `weaveffi.toml` (PascalCased to a
/// legal Swift module) drives it; falling back to the `WeaveFFI` brand.
pub(crate) fn resolve_module_name(api: &ResolvedApi, config: &SwiftConfig) -> String {
    config
        .module_name
        .clone()
        .or_else(|| {
            api.package()
                .and_then(|p| p.name.as_deref())
                .map(ToUpperCamelCase::to_upper_camel_case)
        })
        .unwrap_or_else(|| "WeaveFFI".to_string())
}

/// Render `Package.swift` for the source layout: a `systemLibrary` target
/// over the generated C header plus the Swift wrapper target.
pub(crate) fn render_package_swift(
    module_name: &str,
    c_module: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    // `swift-tools-version` MUST be the very first line of the manifest
    // (Swift 6+ rejects it otherwise), so the WeaveFFI header prelude
    // follows it rather than preceding it.
    format!(
        "// swift-tools-version:5.7\n\
{prelude}import PackageDescription\n\n\
let package = Package(\n    \
    name: \"{name}\",\n    \
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],\n    \
    products: [\n        \
        .library(name: \"{name}\", targets: [\"{name}\"]),\n    \
    ],\n    \
    targets: [\n        \
        .systemLibrary(name: \"{c_name}\"),\n        \
        .target(name: \"{name}\", dependencies: [\"{c_name}\"]),\n    \
    ]\n\
)\n\n\
{trailer}",
        name = swift_str(module_name),
        c_name = swift_str(c_module),
        trailer = render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
    )
}

/// Render `Package.swift` for the packaged layout: the C ABI is consumed
/// through a prebuilt `binaryTarget` xcframework instead of a
/// `systemLibrary`, so installation needs no system lib on the search path.
pub(crate) fn render_packaged_package_swift(
    module_name: &str,
    c_module: &str,
    xcframework: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    format!(
        "// swift-tools-version:5.7\n\
{prelude}import PackageDescription\n\n\
let package = Package(\n    \
    name: \"{name}\",\n    \
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],\n    \
    products: [\n        \
        .library(name: \"{name}\", targets: [\"{name}\"]),\n    \
    ],\n    \
    targets: [\n        \
        .binaryTarget(name: \"{c_name}\", path: \"{xcframework}\"),\n        \
        .target(name: \"{name}\", dependencies: [\"{c_name}\"]),\n    \
    ]\n\
)\n\n\
{trailer}",
        name = swift_str(module_name),
        c_name = swift_str(c_module),
        xcframework = swift_str(xcframework),
        trailer = render_trailer(CommentStyle::DoubleSlash, "Package.swift"),
    )
}

/// Render the `systemLibrary` module map pointing at the generated C header.
///
/// The module map lives at `swift/Sources/C<module>/module.modulemap`, so
/// the C header generated at `<out>/c/<prefix>.h` is three levels up.
pub(crate) fn render_modulemap(c_module: &str, prefix: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    format!(
        "{prelude}module {} [system] {{\n  header \"../../../c/{prefix}.h\"\n  link \"weaveffi\"\n  export *\n}}\n\n{trailer}",
        c_module,
        trailer = render_trailer(CommentStyle::DoubleSlash, "module.modulemap"),
    )
}

/// README for a packaged Swift artifact: it documents assembling the
/// `binaryTarget` xcframework from the bundled per-platform slices, the one
/// step that requires Apple tooling (`lipo` + `xcodebuild`).
pub(crate) fn render_packaged_readme(
    module_name: &str,
    c_module: &str,
    prefix: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {module_name} (Swift)

A SwiftPM package whose C ABI is consumed through a prebuilt `binaryTarget`
xcframework named `{c_module}.xcframework`.

The prebuilt libraries are bundled under `lib/<platform>/`. Assembling them into
an xcframework is the one step that needs Apple tooling (run on macOS):

```bash
# Fuse the macOS arm64 and x86_64 dylibs into one universal binary.
lipo -create \
  lib/darwin-arm64/lib{prefix}.dylib \
  lib/darwin-x64/lib{prefix}.dylib \
  -output lib{prefix}.dylib

# Headers/ must contain {prefix}.h and a module map naming the module {c_module}.
mkdir -p Headers
cp ../c/include/{prefix}.h Headers/
printf 'module {c_module} {{\n  header "{prefix}.h"\n  export *\n}}\n' > Headers/module.modulemap

xcodebuild -create-xcframework \
  -library lib{prefix}.dylib -headers Headers \
  -output {c_module}.xcframework
```

Then `swift build` resolves the binary target with no further setup.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}
