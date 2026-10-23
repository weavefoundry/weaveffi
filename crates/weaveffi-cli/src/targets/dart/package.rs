//! Package files: `pubspec.yaml` and the README for the generated and
//! packaged layouts.

use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

use crate::targets::dart::runtime::bundles_platform;

/// Quote a YAML scalar.
fn yaml_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Render `pubspec.yaml` for package `package`.
pub(crate) fn render_pubspec(identity: &Identity, package: &str, sdk: &str) -> String {
    let mut out = render_prelude(CommentStyle::Hash);
    out.push_str(&format!("name: {package}\n"));
    out.push_str(&format!("version: {}\n", identity.version));
    out.push_str(&format!(
        "description: {}\n",
        yaml_str(&identity.description_or_default())
    ));
    if let Some(homepage) = &identity.homepage {
        out.push_str(&format!("homepage: {}\n", yaml_str(homepage)));
    }
    if let Some(repository) = &identity.repository {
        out.push_str(&format!("repository: {}\n", yaml_str(repository)));
    }
    out.push_str(&format!(
        "\nenvironment:\n  sdk: {}\n\ndependencies:\n  ffi: ^2.1.0\n\n",
        yaml_str(sdk)
    ));
    out.push_str(&render_trailer(CommentStyle::Hash, "pubspec.yaml"));
    out
}

/// Render the README of the generated package.
pub(crate) fn render_readme(identity: &Identity, package: &str, sdk: &str) -> String {
    let (macos, linux, windows) = identity.library_files();
    let env = identity.library_env_var();
    let name = &identity.name;
    format!(
        "{prelude}# {name} (Dart)

`dart:ffi` bindings for the `{name}` native library.

## Usage

Add this package as a dependency (a `path:` dependency works) and import it:

```dart
import 'package:{package}/{package}.dart';
```

The library loads `{macos}` (macOS), `{linux}` (Linux and Android), or
`{windows}` (Windows) from the platform's search path on first use. Set
`{env}` to a full path to load a specific build. On iOS, and as a macOS
fallback, the bindings look the symbols up in the running executable. A
library that can't be loaded, or that doesn't match these bindings, throws a
`NativeLibraryException` from the first call; the next call tries again.

## Objects and callbacks

Each interface wrapper holds one native reference: call `dispose()` when
you're done, or let the garbage collector's finalizer release it.

Callback-interface methods that return a value run synchronously on the
isolate's thread, during a call from Dart; the native library can't call
one from another thread (that call fails without running the method). Void
methods may be called from any thread and are delivered on the event loop.

## Requirements

- Dart SDK `{sdk}`
- The `ffi` package

{trailer}",
        prelude = render_prelude(CommentStyle::Xml),
        trailer = render_trailer(CommentStyle::Xml, "README.md"),
    )
}

/// Render the README of a packaged library that bundles native binaries.
pub(crate) fn render_packaged_readme(identity: &Identity, ctx: &PackageContext) -> String {
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .filter(|p| bundles_platform(*p))
        .map(|p| format!("- `native/{}/`", p.id()))
        .collect();
    format!(
        "{prelude}# {name} (Dart)

`dart:ffi` bindings with prebuilt native libraries bundled under
`native/<platform>/`. The loader tries the running platform's bundled
library (in this package, then under the working directory) before the
system search path; `{env}` overrides all of them.

## Bundled platforms

{platforms}

{trailer}",
        prelude = render_prelude(CommentStyle::Xml),
        name = identity.name,
        env = identity.library_env_var(),
        platforms = platforms.join("\n"),
        trailer = render_trailer(CommentStyle::Xml, "README.md"),
    )
}
