//! Packaging scaffold: `go.mod`, the README flavors, and the `package` hook
//! bundling per-platform shared libraries with a rewritten cgo preamble.

use camino::Utf8Path;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::platform::Platform;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::{render_go, GoConfig};

/// The `(GOOS, GOARCH)` build-constraint tokens for a [`Platform`], used on
/// `#cgo` directive lines.
fn go_build_tags(p: Platform) -> (&'static str, &'static str) {
    match p {
        Platform::MacosArm64 => ("darwin", "arm64"),
        Platform::MacosX64 => ("darwin", "amd64"),
        Platform::LinuxX64 => ("linux", "amd64"),
        Platform::LinuxArm64 => ("linux", "arm64"),
        Platform::WindowsX64 => ("windows", "amd64"),
    }
}

/// The generated `go.mod` for the emitted package.
pub(crate) fn render_go_mod(module_path: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "go.mod");
    // Go 1.23 is required for the standard `iter` package the lazy
    // `iter<T>` wrappers return.
    format!("{prelude}module {module_path}\n\ngo 1.23\n\n{trailer}")
}

/// README for the generate-mode output, describing the manual build setup.
pub(crate) fn render_readme(input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    format!(
        r#"{prelude}# WeaveFFI Go Bindings

Auto-generated Go bindings using CGo.

## Prerequisites

- Go >= 1.23 (the bindings return standard `iter` package sequences)
- A C compiler (gcc or clang) accessible to CGo
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`,
  or `weaveffi.dll`) and the C header (`weaveffi.h`)

## Build

1. Place `libweaveffi.so` (or the platform-specific equivalent) and
   `weaveffi.h` where the linker and CGo can find them. For example,
   install them into `/usr/local/lib` and `/usr/local/include`, or set
   `CGO_LDFLAGS` and `CGO_CFLAGS`:

```sh
export CGO_CFLAGS="-I/path/to/headers"
export CGO_LDFLAGS="-L/path/to/lib -lweaveffi"
```

2. Build or run your Go project that imports this module:

```sh
go build ./...
```

## How It Works

The generated `weaveffi.go` file uses a CGo preamble to `#include "weaveffi.h"`
and link against `-lweaveffi`. Each API function is exposed as an idiomatic Go
function that marshals arguments to C types, calls the C ABI function, and
converts the result back to Go types. Records, rich enums, optionals, lists,
and maps cross the boundary serialized in the WeaveFFI value-buffer format.
Errors are returned as Go `error` values.

{trailer}"#
    )
}

/// README for a packaged Go module that bundles per-platform libraries.
fn render_packaged_readme(ctx: &PackageContext, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# WeaveFFI (Go)

Auto-generated cgo bindings with a prebuilt shared library bundled for each
platform under `lib/<platform>/`. The cgo preamble adds the matching
`${{SRCDIR}}`-relative library search path and rpath per GOOS/GOARCH, so
`go build` links the right library with no manual `CGO_LDFLAGS`.

The C ABI header is expected at `../c/include/` (package the `c` target
alongside Go, for example `weaveffi package --target c,go`).

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

/// The full packaged file set: the Go source with a self-contained cgo
/// preamble, `go.mod`, the packaged README, and one bundled shared library
/// per platform.
pub(crate) fn package_files(
    api: &ResolvedApi,
    model: &BindingModel,
    ctx: &PackageContext,
    out_dir: &Utf8Path,
    config: &GoConfig,
) -> Vec<PackagedFile> {
    let dir = out_dir.join("go");
    let input_basename = config.input_basename();
    let prefix = config.prefix();
    let link_name = pkg::resolve(api, None, Some(input_basename)).ident_name();
    let module_path = pkg::resolve(
        api,
        config.module_path.as_deref(),
        config.input_basename.as_deref(),
    )
    .name;

    // Expand the single generate-mode `#cgo LDFLAGS` line into a
    // self-contained, relocatable set: a header include path plus per
    // GOOS/GOARCH library search + rpath directives (all `${SRCDIR}`
    // relative). cgo selects the matching line at build time.
    let original = format!("#cgo LDFLAGS: -l{link_name}\n");
    let mut cgo = String::from("#cgo CFLAGS: -I${SRCDIR}/../c/include\n");
    for nb in &ctx.binaries.binaries {
        let (goos, goarch) = go_build_tags(nb.platform);
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
    cgo.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
    let go_src = render_go(
        api,
        model,
        prefix,
        config.strip_module_prefix,
        input_basename,
    )
    .replace(&original, &cgo);

    let mut files = vec![
        PackagedFile::text(dir.join("weaveffi.go"), go_src),
        PackagedFile::text(
            dir.join("go.mod"),
            render_go_mod(&module_path, input_basename),
        ),
        PackagedFile::text(
            dir.join("README.md"),
            render_packaged_readme(ctx, input_basename),
        ),
    ];
    for nb in &ctx.binaries.binaries {
        let dest = dir
            .join("lib")
            .join(nb.platform.id())
            .join(ctx.binaries.bundled_filename(nb.platform));
        files.push(PackagedFile::copy(dest, nb.source.clone()));
    }
    files
}
