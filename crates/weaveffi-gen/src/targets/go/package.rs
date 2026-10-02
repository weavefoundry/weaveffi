//! Module scaffold: `go.mod`, the README flavors, and the `package` hook
//! bundling per-platform shared libraries with a rewritten cgo preamble.

use crate::package::{PackageContext, PackagedFile};
use crate::platform::{NativeBinary, Platform};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::go::{render_files, GoConfig, Names};

/// The `(GOOS, GOARCH)` build-constraint tokens for a [`Platform`], used on
/// `#cgo` directive lines, or `None` for a platform the cgo package has no
/// slot for (Android and wasm32 binaries are skipped when packaging).
fn go_build_tags(p: Platform) -> Option<(&'static str, &'static str)> {
    match p {
        Platform::MacosArm64 => Some(("darwin", "arm64")),
        Platform::MacosX64 => Some(("darwin", "amd64")),
        Platform::LinuxX64 => Some(("linux", "amd64")),
        Platform::LinuxArm64 => Some(("linux", "arm64")),
        Platform::WindowsX64 => Some(("windows", "amd64")),
        Platform::AndroidArm64
        | Platform::AndroidX64
        | Platform::Wasm32
        | Platform::IosArm64
        | Platform::IosSimArm64
        | Platform::IosSimX64 => None,
    }
}

/// The bundled binaries this package has a `#cgo` slot for, in the binary
/// set's order.
fn bundled<'a>(
    ctx: &'a PackageContext<'a>,
) -> impl Iterator<Item = (&'a NativeBinary, &'static str, &'static str)> {
    ctx.binaries
        .binaries
        .iter()
        .filter_map(|nb| go_build_tags(nb.platform).map(|(goos, goarch)| (nb, goos, goarch)))
}

/// The generated `go.mod` for the emitted module.
pub(crate) fn render_go_mod(module_path: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "go.mod");
    // Go 1.23 is required for the standard `iter` package the lazy
    // `iter<T>` wrappers return.
    format!("{prelude}module {module_path}\n\ngo 1.23\n\n{trailer}")
}

/// README for the generate-mode output.
pub(crate) fn render_readme(names: &Names, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let Names {
        module_path,
        package,
        library,
        header,
        ..
    } = names;
    format!(
        r#"{prelude}# {package} (Go)

cgo bindings for the `{library}` native library.

## Build

The module ships its own copy of the C header (`{header}`) and links with
`-l{library}`. Point the linker at the directory holding `lib{library}.so`,
`lib{library}.dylib`, or `{library}.dll`, and make the library findable at
run time:

```sh
export CGO_LDFLAGS="-L/path/to/lib"
export LD_LIBRARY_PATH="/path/to/lib"   # DYLD_LIBRARY_PATH on macOS
go build ./...
```

Depend on the module from your own project with a `require` (and, for a
local checkout, a `replace`) of `{module_path}`, then import it:

```go
import "{module_path}"
```

Go 1.23 or newer is required.

## Usage notes

- Records are plain structs and rich enums are sealed interfaces; both cross
  the boundary by value.
- Interfaces are reference-counted objects. Call `Close` when you're done
  with one (a finalizer releases it otherwise); it's safe to call twice or
  while a call is in flight.
- Async functions take a `context.Context` first. Cancellable ones cancel
  the native call when the context is done.
- Callback interfaces are Go interfaces you implement. The library may call
  them from any thread, and a panic is reported to the native caller as a
  foreign error instead of crashing.
- Loading checks the library's ABI revision and contract checksums and
  panics with the stale module's name on a mismatch.

{trailer}"#
    )
}

/// README for a packaged Go module that bundles per-platform libraries.
fn render_packaged_readme(ctx: &PackageContext, names: &Names, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = bundled(ctx)
        .map(|(nb, _, _)| format!("- `lib/{}/`", nb.platform.id()))
        .collect();
    let platform_list = platforms.join("\n");
    let package = &names.package;
    format!(
        r#"{prelude}# {package} (Go)

cgo bindings with a prebuilt shared library bundled for each platform under
`lib/<platform>/`. The cgo preamble adds the matching `${{SRCDIR}}`-relative
library search path and rpath per GOOS/GOARCH, so `go build` links the right
library with no manual `CGO_LDFLAGS`.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

/// The full packaged file set: the generated module with a self-contained
/// cgo preamble plus one bundled shared library per desktop platform.
/// Binaries for platforms without a `GOOS,GOARCH` cgo slot (Android, wasm32)
/// are skipped.
pub(crate) fn package_files(
    api: &ResolvedApi,
    model: &BindingModel,
    ctx: &PackageContext,
    out_dir: &Utf8Path,
    config: &GoConfig,
) -> Vec<PackagedFile> {
    let dir = out_dir.join("go");
    let input_basename = config.input_basename();
    let names = Names::new(api, config);
    let library = &names.library;

    // Expand the single generate-mode `#cgo LDFLAGS` line into per
    // GOOS/GOARCH library search and rpath directives (all `${SRCDIR}`
    // relative). cgo selects the matching line at build time.
    let original = format!("#cgo LDFLAGS: -l{library}\n");
    let mut cgo = String::new();
    for (nb, goos, goarch) in bundled(ctx) {
        let id = nb.platform.id();
        if nb.platform == Platform::WindowsX64 {
            cgo.push_str(&format!(
                "#cgo {goos},{goarch} LDFLAGS: -L${{SRCDIR}}/lib/{id}\n"
            ));
        } else {
            cgo.push_str(&format!(
                "#cgo {goos},{goarch} LDFLAGS: -L${{SRCDIR}}/lib/{id} -Wl,-rpath,${{SRCDIR}}/lib/{id}\n"
            ));
        }
    }
    cgo.push_str(&original);

    let mut files: Vec<PackagedFile> = render_files(api, model, config)
        .into_iter()
        .map(|(name, contents)| {
            let contents = match name.as_str() {
                "bindings.go" => contents.replace(&original, &cgo),
                "README.md" => render_packaged_readme(ctx, &names, input_basename),
                _ => contents,
            };
            PackagedFile::text(dir.join(name), contents)
        })
        .collect();
    for (nb, _, _) in bundled(ctx) {
        let dest = dir
            .join("lib")
            .join(nb.platform.id())
            .join(ctx.binaries.bundled_filename(nb.platform));
        files.push(PackagedFile::copy(dest, nb.source.clone()));
    }
    files
}
