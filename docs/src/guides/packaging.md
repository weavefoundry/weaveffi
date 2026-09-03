# Packaging and Distribution

## Overview

`weaveffi generate` emits binding *source*: the consumer still has to compile
it or point it at a native library. `weaveffi package` goes one step further
and assembles ready-to-publish packages that bundle a prebuilt native library
for each target platform, laid out the idiomatic way each ecosystem expects.
The goal is that `dotnet add package`, `pip install`, `gem install`,
`npm install`, and friends "just work" with no local toolchain on a supported
platform.

```bash
weaveffi package api.yml --binaries ./prebuilt --target dotnet,python,ruby -o dist
```

## Choosing where the native libraries come from

A package can only bundle libraries you have already built.
`weaveffi package` gets them one of two ways:

- `--binaries <dir>`: a directory of prebuilt libraries laid out as
  `<dir>/<platform>/<library>`. This is the path CI uses, building each
  platform on its own runner and collecting the results.
- `--build <crate>`: cross-compile the given Cargo package as a `cdylib` for
  each platform's Rust target triple. Convenient locally, but every target
  needs its rustup target and a working cross-linker installed
  (`rustup target add aarch64-unknown-linux-gnu`, and so on; the Android
  targets additionally need the NDK's linker on `PATH`).

The two are mutually exclusive. A `--build` run fails with the missing
`rustup target add` command when a platform's Rust target isn't installed.

### The `--binaries` layout

Each platform gets a subdirectory named for its platform id, holding that
platform's shared library (or, for `wasm32`, the `.wasm` module):

```text
prebuilt/
  darwin-arm64/libcontacts.dylib
  darwin-x64/libcontacts.dylib
  linux-x64/libcontacts.so
  linux-arm64/libcontacts.so
  windows-x64/contacts.dll
  android-arm64/libcontacts.so
  android-x64/libcontacts.so
  wasm32/contacts.wasm
```

A platform with no subdirectory is skipped with a warning, so a partial
matrix still produces artifacts for what is available. When a platform
directory holds more than one library, name the one to bundle after the
resolved package identity (for example `libcontacts.dylib`) to disambiguate.

## The platform matrix

Every platform WeaveFFI can bundle, with the identifier each ecosystem uses
for it. A blank cell means that ecosystem has no slot for the platform, and a
packaging backend **skips** a binary it has no slot for (a wheel has no wasm
tag; a NuGet package has no Android RID) rather than failing.

| Platform id | OS / arch | Rust target | NuGet RID | Node `os`/`cpu` | Python tag | Ruby platform | Android ABI |
|-------------|-----------|-------------|-----------|-----------------|------------|---------------|-------------|
| `darwin-arm64` | macOS arm64 | `aarch64-apple-darwin` | `osx-arm64` | `darwin`/`arm64` | `macosx_11_0_arm64` | `arm64-darwin` | |
| `darwin-x64` | macOS x64 | `x86_64-apple-darwin` | `osx-x64` | `darwin`/`x64` | `macosx_10_12_x86_64` | `x86_64-darwin` | |
| `linux-x64` | Linux x64 glibc | `x86_64-unknown-linux-gnu` | `linux-x64` | `linux`/`x64` | `manylinux2014_x86_64` | `x86_64-linux` | |
| `linux-arm64` | Linux arm64 glibc | `aarch64-unknown-linux-gnu` | `linux-arm64` | `linux`/`arm64` | `manylinux2014_aarch64` | `aarch64-linux` | |
| `windows-x64` | Windows x64 | `x86_64-pc-windows-msvc` | `win-x64` | `win32`/`x64` | `win_amd64` | `x64-mingw-ucrt` | |
| `android-arm64` | Android arm64 | `aarch64-linux-android` | | | | | `arm64-v8a` |
| `android-x64` | Android x64 (emulator) | `x86_64-linux-android` | | | | | `x86_64` |
| `wasm32` | WebAssembly | `wasm32-unknown-unknown` | | | | | |

The first five rows are the **desktop** matrix; every dynamic-library
ecosystem (NuGet, npm, wheels, gems, C/C++, Go, Swift, Dart, desktop JVM)
bundles those. The Android rows are consumed only by the `kotlin` target, and
`wasm32` only by the `wasm` target.

Restrict the build with `--platforms` (a comma-separated list of platform
ids); the default is the full matrix. Restrict the languages with `--target`
exactly as in `weaveffi generate`. The command prints one line per target it
packaged and a `note:` listing any target that produced nothing, either
because it has no binary packaging at all or because no binary was found for a
platform it ships:

```text
Packaging 'contacts' for platforms: darwin-arm64, linux-x64, wasm32
  dotnet: 3 file(s), 2 bundled binary(ies)
  wasm: 4 file(s), 1 bundled binary(ies)
Packaged 2 target(s) into dist
```

The `wasm` target is the one that can come up empty on a desktop-only matrix:
with no `wasm32/` binary it is skipped with a note, and in Emscripten mode it
ships glue only and never bundles a binary. Every other target packages as
long as at least one of its platforms is present.

## Per-ecosystem layout

Each target lays the bundled libraries out where its ecosystem resolves
native code automatically.

### .NET (`dotnet`)

A single NuGet-ready project with libraries under `runtimes/<rid>/native/`,
the layout NuGet selects at restore time. The `[DllImport]` library name is
rebound from the WeaveFFI brand to the bundled library's base name, and the
`.csproj` packs the `runtimes/` tree. Just `dotnet add package`.

### Python (`python`)

One wheel-ready tree per platform under `python/<platform>/`, with the library
bundled inside the import package. The loader prefers the bundled library, so
no `WEAVEFFI_LIBRARY` or system install is needed. The generated `setup.py`
forces a non-pure (platform-tagged) wheel; build it with
`python -m build --wheel` and tag it for the target platform before
publishing.

### Ruby (`ruby`)

One precompiled platform gem per platform under `ruby/<platform>/`, with
`s.platform` set and the library bundled under `lib/native/`. The `ffi` loader
prefers the bundled library.

### Node.js (`node`)

The idiomatic `optionalDependencies` layout: a main package that depends on
one per-platform package per target (each gated by npm `os`/`cpu`, so only the
matching one installs) under `node/npm/<name>-<os>-<cpu>/`, each bundling its
prebuilt library. Because the Node binding is an N-API addon, the thin addon
is still compiled at install (`node-gyp rebuild`) and links the prebuilt
library from the selected platform package, so no Rust toolchain is needed; a
C compiler and the generated C header (package the `c` target alongside) are.

### WebAssembly (`wasm`)

An npm package under `wasm/` containing the ES-module loader
(`<module_name>.js`), its TypeScript declarations (`<module_name>.d.ts`), a
README, and the `wasm32` binary copied in as `<lib_name>.wasm`. The
`package.json` lists exactly those files so `npm publish` ships nothing else.
Consumers call the loader's `load…` function with the bundled `.wasm` URL (or
a `BufferSource` or compiled `WebAssembly.Module`) and get the API object back
as a `Promise`.

Build the binary with `cargo build --release --target wasm32-unknown-unknown`
(the producer crate needs `crate-type = ["cdylib"]`) and place it under
`prebuilt/wasm32/`. In Emscripten mode (`[generators.wasm] emscripten =
true`) the package is glue only: the consumer links the module into their own
Emscripten build, so no binary is bundled and the `wasm32/` directory is not
consulted.

### Kotlin (`kotlin`)

A Gradle Android library module under `kotlin/`: `settings.gradle.kts`,
`build.gradle.kts` (`com.android.library` plus the Kotlin Android plugin,
`minSdk = 33` because the wrappers' disposal backstop is
`java.lang.ref.Cleaner`), the Kotlin wrapper under
`src/main/kotlin/<package path>/WeaveFFI.kt`, and the JNI shim under
`src/main/cpp/` with its own `CMakeLists.txt` and a bundled copy of the C
header under `src/main/cpp/include/`.

The binaries land in two places depending on the platform:

- **Android** (`android-arm64`, `android-x64`) libraries are copied to
  `src/main/jniLibs/<abi>/` (`arm64-v8a`, `x86_64`), which the packaged
  `build.gradle.kts` registers as a JNI source set. The CMake script links the
  shim against the bundled library for the ABI being built, so an app that
  depends on the module gets both `.so`s in its APK with no further setup.
- **Desktop JVM** libraries (the five desktop platforms) are copied to
  `src/main/resources/natives/<platform id>/`. At runtime the Kotlin loader
  picks `natives/<os>-<arch>/` from `os.name` and `os.arch`, extracts the
  producer library and the shim from the classpath into a temporary
  directory, and `System.load`s them, falling back to
  `System.loadLibrary("<lib>_jni")` on the `java.library.path` when the
  resources are absent. When building the shim for a desktop host, pass
  `-DWEAVEFFI_PLATFORM_ID=<id>` to CMake so it links the right bundled
  library; the generated README has the exact command.

The `.wasm` binary is ignored by this target. One caveat: the generated JNI
glue always defines `JNI_OnLoad` (it caches the `JavaVM*`, checks the ABI
revision, and resolves the classes callback dispatch and async exception
routing need). Each generated module must therefore link into its own shared
library; compiling the glue for two modules into one `.so` would collide on
the duplicate `JNI_OnLoad` symbol.

### Swift (`swift`)

A SwiftPM package that consumes its C ABI through a `binaryTarget`
xcframework. The prebuilt desktop libraries are bundled under
`lib/<platform>/`; assembling them into the xcframework is the one step that
needs Apple tooling (`lipo` plus `xcodebuild -create-xcframework`, run on
macOS). The generated `README.md` includes the exact recipe.

### C and C++ (`c`, `cpp`)

The header (`include/`) plus a prebuilt library under `lib/<platform>/`, with
a `CMakeLists.txt` that selects the right library for the host and exposes it
as an imported target. `add_subdirectory` and link. The `c` package copies
every binary present (Android and `wasm32` included, since a C consumer may be
an NDK or Emscripten build); the `cpp` package bundles the desktop platforms
only, which are the ones its packaged `CMakeLists.txt` can select among.

### Go (`go`)

A Go module that bundles a library per desktop platform under
`lib/<platform>/`. The cgo preamble adds the matching `${SRCDIR}`-relative
library search path and rpath per GOOS/GOARCH, so `go build` links the right
library with no manual `CGO_LDFLAGS`. The C ABI header is expected at
`../c/include/`, so package the `c` target alongside Go
(`weaveffi package --target c,go`).

### Dart (`dart`)

A pub package under `dart/` with the desktop libraries under
`native/<platform id>/`. The packaged `dart:ffi` loader tries the bundled
library for the host OS first, then the bare system name; `WEAVEFFI_LIBRARY`
still overrides. Android libraries are not bundled here: on Flutter they ship
through the app's `jniLibs`, and a `.wasm` module can't be opened with
`DynamicLibrary`.

## Continuous integration recipe

In CI the cleanest approach is to build each platform's library on a runner
of that platform (native builds avoid cross-linker setup), collect the
results into the `--binaries` layout, then run `weaveffi package` once. The
matrix below builds a Cargo producer crate (`my-producer`, declaring
`crate-type = ["cdylib"]`) and uploads each library under its platform id,
then a final job assembles the packages. The Android and wasm rows
cross-compile from Linux: the wasm target needs only `rustup target add`, and
the Android targets need the NDK that GitHub's Ubuntu runners ship, exposed
to Cargo through `CARGO_TARGET_<TRIPLE>_LINKER`.

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
          - { platform: darwin-arm64,  runner: macos-14,         target: aarch64-apple-darwin,       lib: libmy_producer.dylib }
          - { platform: darwin-x64,    runner: macos-13,         target: x86_64-apple-darwin,        lib: libmy_producer.dylib }
          - { platform: linux-x64,     runner: ubuntu-latest,    target: x86_64-unknown-linux-gnu,   lib: libmy_producer.so }
          - { platform: linux-arm64,   runner: ubuntu-24.04-arm, target: aarch64-unknown-linux-gnu,  lib: libmy_producer.so }
          - { platform: windows-x64,   runner: windows-latest,   target: x86_64-pc-windows-msvc,     lib: my_producer.dll }
          - { platform: android-arm64, runner: ubuntu-latest,    target: aarch64-linux-android,      lib: libmy_producer.so }
          - { platform: android-x64,   runner: ubuntu-latest,    target: x86_64-linux-android,       lib: libmy_producer.so }
          - { platform: wasm32,        runner: ubuntu-latest,    target: wasm32-unknown-unknown,     lib: my_producer.wasm }
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - name: Point Cargo at the NDK linker
        if: startsWith(matrix.platform, 'android-')
        run: |
          bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
          case "${{ matrix.target }}" in
            aarch64-linux-android) echo "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$bin/aarch64-linux-android33-clang" >> "$GITHUB_ENV" ;;
            x86_64-linux-android)  echo "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$bin/x86_64-linux-android33-clang" >> "$GITHUB_ENV" ;;
          esac
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
      - run: weaveffi package api.yml --binaries prebuilt --target dotnet,python,node,ruby,kotlin,wasm -o dist
      # ... then publish each package with `dotnet nuget push`, `npm publish`, etc.
```

A platform you can't build (no runner, missing target) can simply be dropped
from the matrix; `weaveffi package` warns and produces artifacts for whatever
is present. Each target then bundles the subset of platforms it has a slot
for, so a matrix without the Android rows still yields a `kotlin` package
(desktop JVM only) and one without `wasm32` yields everything except `wasm`.
