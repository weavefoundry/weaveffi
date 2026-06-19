# Packaging and Distribution

## Overview

`weaveffi generate` emits binding *source*: the consumer still has to compile it
or point it at a native library. `weaveffi package` goes one step further and
assembles ready-to-publish packages that bundle a prebuilt native library for
each target platform, laid out the idiomatic way each ecosystem expects. The
goal is that `dotnet add package`, `pip install`, `gem install`, `npm install`,
and friends "just work" with no local toolchain on a supported platform.

```bash
weaveffi package api.yml --binaries ./prebuilt --target dotnet,python,ruby -o dist
```

## Choosing where the native libraries come from

A package can only bundle libraries you have already built. `weaveffi package`
gets them one of two ways:

- `--binaries <dir>`: a directory of prebuilt libraries laid out as
  `<dir>/<platform>/<library>`. This is the path CI uses, building each platform
  on its own runner and collecting the results.
- `--build <crate>`: cross-compile the given Cargo package as a `cdylib` for each
  platform's Rust target triple. Convenient locally, but every target needs its
  rustup target and a working cross-linker installed
  (`rustup target add aarch64-unknown-linux-gnu`, and so on).

The two are mutually exclusive. Before a `--build` run, `weaveffi doctor --target
package` reports which producer cross-targets are installed (and the
`rustup target add` command for any that are missing), and exits non-zero if any
are absent so it can gate the build in CI.

### The `--binaries` layout

Each platform gets a subdirectory named for its platform id, holding that
platform's shared library:

```text
prebuilt/
  darwin-arm64/libcontacts.dylib
  darwin-x64/libcontacts.dylib
  linux-x64/libcontacts.so
  linux-arm64/libcontacts.so
  windows-x64/contacts.dll
```

A platform with no subdirectory is skipped with a warning, so a partial matrix
still produces artifacts for what is available. When a platform directory holds
more than one library, name the one to bundle after the resolved package
identity (for example `libcontacts.dylib`) to disambiguate.

## The v1 platform matrix

| Platform id    | OS / arch       | Rust target                  | NuGet RID   | Node `os`/`cpu` | Python tag             | Ruby platform   |
| -------------- | --------------- | ---------------------------- | ----------- | --------------- | ---------------------- | --------------- |
| `darwin-arm64` | macOS arm64     | `aarch64-apple-darwin`       | `osx-arm64` | `darwin`/`arm64`| `macosx_11_0_arm64`    | `arm64-darwin`  |
| `darwin-x64`   | macOS x64       | `x86_64-apple-darwin`        | `osx-x64`   | `darwin`/`x64`  | `macosx_10_12_x86_64`  | `x86_64-darwin` |
| `linux-x64`    | Linux x64 glibc | `x86_64-unknown-linux-gnu`   | `linux-x64` | `linux`/`x64`   | `manylinux2014_x86_64` | `x86_64-linux`  |
| `linux-arm64`  | Linux arm64 glibc | `aarch64-unknown-linux-gnu`| `linux-arm64`| `linux`/`arm64`| `manylinux2014_aarch64`| `aarch64-linux` |
| `windows-x64`  | Windows x64     | `x86_64-pc-windows-msvc`     | `win-x64`   | `win32`/`x64`   | `win_amd64`            | `x64-mingw-ucrt`|

Restrict the build with `--platforms` (a comma-separated list of platform ids);
the default is the full matrix. Restrict the languages with `--target` exactly
as in `weaveffi generate`.

## Per-ecosystem layout

Each target lays the bundled libraries out where its ecosystem resolves native
code automatically.

### .NET (`dotnet`)

A single NuGet-ready project with libraries under `runtimes/<rid>/native/`, the
layout NuGet selects at restore time. The `[DllImport]` library name is rebound
from the WeaveFFI brand to the bundled library's base name, and the `.csproj`
packs the `runtimes/` tree. Just `dotnet add package`.

### Python (`python`)

One wheel-ready tree per platform under `python/<platform>/`, with the library
bundled inside the import package. The loader prefers the bundled library, so no
`WEAVEFFI_LIBRARY` or system install is needed. The generated `setup.py` forces a
non-pure (platform-tagged) wheel; build it with `python -m build --wheel` and tag
it for the target platform before publishing.

### Ruby (`ruby`)

One precompiled platform gem per platform under `ruby/<platform>/`, with
`s.platform` set and the library bundled under `lib/native/`. The `ffi` loader
prefers the bundled library.

### Node.js (`node`)

The idiomatic `optionalDependencies` layout: a main package that depends on one
per-platform package per target (each gated by npm `os`/`cpu`, so only the
matching one installs) under `node/npm/<name>-<os>-<cpu>/`, each bundling its
prebuilt library. Because the Node binding is an N-API addon, the thin addon is
still compiled at install (`node-gyp rebuild`) and links the prebuilt library
from the selected platform package, so no Rust toolchain is needed; a C compiler
and the generated C header (package the `c` target alongside) are.

### Swift (`swift`)

A SwiftPM package that consumes its C ABI through a `binaryTarget` xcframework.
The prebuilt libraries are bundled under `lib/<platform>/`; assembling them into
the xcframework is the one step that needs Apple tooling (`lipo` plus
`xcodebuild -create-xcframework`, run on macOS). The generated `README.md`
includes the exact recipe.

### C and C++ (`c`, `cpp`)

The header (`include/`) plus a prebuilt library for every platform under
`lib/<platform>/`, with a `CMakeLists.txt` that selects the right library for the
host and exposes it as an imported target. `add_subdirectory` and link.

### Go (`go`)

A Go module that bundles a library per platform under `lib/<platform>/`. The cgo
preamble adds the matching `${SRCDIR}`-relative library search path and rpath per
GOOS/GOARCH, so `go build` links the right library with no manual `CGO_LDFLAGS`.
The C ABI header is expected at `../c/include/`, so package the `c` target
alongside Go (`weaveffi package --target c,go`).

## Continuous integration recipe

In CI the cleanest approach is to build each platform's library on a runner of
that platform (native builds avoid cross-linker setup), collect the results into
the `--binaries` layout, then run `weaveffi package` once. The matrix below
builds a Cargo producer crate (`my-producer`, declaring
`crate-type = ["cdylib"]`) and uploads each library under its platform id, then a
final job assembles the packages.

```yaml
name: package
on:
  push:
    tags: ["v*"]

jobs:
  build:
    strategy:
      matrix:
        include:
          - { platform: darwin-arm64,  runner: macos-14,        target: aarch64-apple-darwin,      lib: libmy_producer.dylib }
          - { platform: darwin-x64,    runner: macos-13,        target: x86_64-apple-darwin,       lib: libmy_producer.dylib }
          - { platform: linux-x64,     runner: ubuntu-latest,   target: x86_64-unknown-linux-gnu,  lib: libmy_producer.so }
          - { platform: linux-arm64,   runner: ubuntu-24.04-arm, target: aarch64-unknown-linux-gnu, lib: libmy_producer.so }
          - { platform: windows-x64,   runner: windows-latest,  target: x86_64-pc-windows-msvc,    lib: my_producer.dll }
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - run: cargo build --release -p my-producer --target ${{ matrix.target }}
      - run: |
          mkdir -p "prebuilt/${{ matrix.platform }}"
          cp "target/${{ matrix.target }}/release/${{ matrix.lib }}" "prebuilt/${{ matrix.platform }}/"
        shell: bash
      - uses: actions/upload-artifact@v4
        with:
          name: prebuilt-${{ matrix.platform }}
          path: prebuilt/

  package:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          pattern: prebuilt-*
          path: prebuilt
          merge-multiple: true
      - run: cargo install weaveffi-cli
      - run: weaveffi package api.yml --binaries prebuilt --target dotnet,python,node,ruby -o dist
      # ... then publish each package with `dotnet nuget push`, `npm publish`, etc.
```

A platform you can't build (no runner, missing target) can simply be dropped
from the matrix; `weaveffi package` warns and produces artifacts for whatever is
present.

## Targets that do not bundle binaries yet

`wasm` and `android` are skipped (with a note) because they need a different
artifact model than the native matrix: a WebAssembly module is a single portable
binary, and Android ships per-ABI libraries (`arm64-v8a`, and so on) that map to
Android-specific targets rather than the desktop/server platforms above. Use
`weaveffi generate` for their source bindings.
