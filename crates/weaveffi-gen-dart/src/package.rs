//! Package-file emission: `pubspec.yaml` and the README variants for the
//! generated and packaged layouts.

use weaveffi_core::package::PackageContext;
use weaveffi_core::pkg::ResolvedPackage;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::runtime::bundles_platform;

/// Render the `pubspec.yaml` manifest for the generated Dart package.
pub(crate) fn render_pubspec(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pubspec.yaml");
    let name = package.ident_name();
    let version = &package.version;
    let description = package.description_or_default();
    let mut meta = format!("description: {description}\n");
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        meta.push_str(&format!("homepage: {homepage}\n"));
    }
    format!(
        "{prelude}name: {name}\n\
         version: {version}\n\
         {meta}\
         environment:\n\
         \x20 sdk: '>=3.1.0 <4.0.0'\n\
         dependencies:\n\
         \x20 ffi: ^2.0.0\n\n\
         {trailer}"
    )
}

/// Render the README for a generated (unpackaged) Dart artifact.
pub(crate) fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let import_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Dart)

Auto-generated Dart bindings using `dart:ffi`.

## Usage

1. Place the compiled shared library (`libweaveffi.dylib`, `libweaveffi.so`,
   or `weaveffi.dll`) where the Dart process can find it.

2. Add this package as a dependency and import the bindings:

```dart
import 'package:{import_name}/weaveffi.dart';
```

3. Call the generated functions directly. The bindings use `dart:ffi` to load
   the native library at runtime via `DynamicLibrary.open` and resolve symbols
   with `lookupFunction`.

## Objects and callbacks

Interface objects are reference counted by the native library. Each Dart
wrapper holds one reference: call `dispose()` when you are done with it, or let
the garbage collector's finalizer release it. Objects nested inside records and
collections follow the same rule.

Callback interfaces are abstract classes: implement one and pass an instance to
any function that takes it. Methods are bound with `NativeCallable.isolateLocal`,
so the native library may only invoke them on the isolate's own thread (which
holds when it calls them synchronously during a call from Dart). An exception
thrown by an implementation aborts the native call and surfaces to the original
Dart caller as a `WeaveFFIException` with `WeaveFFIException.foreignCode`.

## Requirements

- Dart SDK >= 3.1.0 (for `NativeCallable`)
- The `ffi` package (`^2.0.0`) for `Utf8` and `calloc` helpers.

{trailer}"#
    )
}

/// Render the README for a packaged Dart artifact that bundles native
/// libraries.
pub(crate) fn render_packaged_readme(
    package: &ResolvedPackage,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = package.name.clone();
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .filter(|p| bundles_platform(*p))
        .map(|p| format!("- `native/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {name} (Dart)

Auto-generated `dart:ffi` bindings with prebuilt native libraries bundled under
`native/<platform>/`. The loader prefers a bundled library (resolved relative to
the working directory) and falls back to the system search path;
`WEAVEFFI_LIBRARY` overrides both.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}
