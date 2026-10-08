# Packaging

`weaveffi generate` emits binding source. Shipping it takes two more steps:
`weaveffi build` cross-compiles the Rust producer once per platform, and
`weaveffi package` turns those builds into the artifacts each ecosystem
installs: wheels, npm tarballs, gems, a SwiftPM package with its
`XCFramework`, a NuGet package, and so on. Consumers install them with
`pip`, `npm`, `gem`, SwiftPM, or `dotnet` and need no compiler.

```bash
weaveffi build --platforms darwin-arm64,ios-arm64,ios-sim-arm64
weaveffi package --target python,node,swift
```

Both read `[project]`, `[package]`, and `[build]` from `weaveffi.toml` (or
take an explicit input), and every artifact loads the library named by the
identity's `library` field.

## Build

`weaveffi build` finds the producer crate with `cargo metadata` (the
project's crate, `[build] manifest`, or `--manifest-path`) and runs
`cargo rustc --lib` once per platform with the artifact kinds that platform
needs, so the crate's own `crate-type` doesn't matter. A Rust producer's API
is then read from the first library it built (see [Library
Mode](extract.md)), so nothing is compiled twice:

| Platform | What `build` produces |
|----------|------------------------|
| macOS | the `cdylib`, with install name `@rpath/lib{library}.dylib`, plus the `staticlib` an `XCFramework` slice is made from |
| iOS | the `staticlib` |
| Linux, Android | the `cdylib`, with soname `lib{library}.so` (Android also 16 KB page-aligned) |
| Windows | the DLL and its import library |
| `wasm32` | the `.wasm` module, linked with `--export-table --growable-table` |

It sets `MACOSX_DEPLOYMENT_TARGET` and `IPHONEOS_DEPLOYMENT_TARGET` from
`[build]`, and for Android it finds the NDK (`ANDROID_NDK_HOME`,
`ANDROID_NDK_ROOT`, or the newest `ndk/<version>` of the Android SDK) and
points Cargo's linker and the `cc` crate at the NDK's clang for the
configured API level. Everything lands in the Cargo target directory:

```text
target/weaveffi/
  darwin-arm64/  libkvstore.dylib  libkvstore.a  kvstore_node.node  libkvstore_jni.dylib
  ios-arm64/     libkvstore.a
  android-arm64/ libkvstore.so  libkvstore_jni.so
  wasm32/        kvstore.wasm
```

The Node.js and Kotlin targets reach the C ABI through C glue of their own
(an N-API addon and a JNI shim). When those targets are selected (`--target`,
else `[project] targets`, else all), `build` compiles the glue next to the
library so packages ship it prebuilt:

- **Node.js addon**, against the running Node.js's headers, for each
  requested desktop platform the host can compile: both macOS architectures
  on a macOS host, and the host's own architecture on a Linux host. Windows
  isn't prebuilt (it links `node.lib`); the package compiles it at install
  time.
- **JNI shim** for every Android ABI (with the NDK) and for the same desktop
  platforms as the addon (with the JDK headers from `JAVA_HOME`); never for
  `windows-x64`, where the generated CMake project builds it.

Glue a host can't compile is skipped with a warning, and the package falls
back to compiling it. Every platform is checked before anything compiles, so
a missing rustup target (`rustup target add ...`), a missing NDK, or a
platform this host can't build (Apple platforms need macOS, Windows needs
Windows) fails at once with every problem listed.

## Package

`weaveffi package` runs `build` for the requested platforms, then writes
each target's artifacts to the dist directory (`-o`, else `[package] dist`,
else `dist`). With `--binaries <dir>`, it skips the build and reads a
directory laid out like `target/weaveffi/` instead; that's how CI hands
libraries built on separate runners to one packaging job, and how a
producer that isn't a Rust crate is packaged. A Rust producer's API is read
from the first library in the directory.

| Target | Artifacts | Publish with |
|--------|-----------|--------------|
| `python` | one `py3-none-{platform}.whl` per desktop platform, the library inside the import package | `twine upload dist/python/*.whl` |
| `node` | `{name}-{os}-{cpu}-{version}.tgz` per desktop platform (library and prebuilt addon, gated by npm `os`/`cpu`) plus `{name}-{version}.tgz`, which lists them in `optionalDependencies` | `npm publish <tgz>` for each, platform packages first |
| `wasm` | `{name}-{version}.tgz` with the `.wasm` module | `npm publish <tgz>` |
| `ruby` | one `{gem}-{version}-{platform}.gem` per desktop platform | `gem push <gem>` |
| `swift` | `C{Module}.xcframework.zip` (static libraries for every Apple platform built), its `.sha256`, and the SwiftPM package `swift/{Module}/` whose binary target points at the archive's URL and checksum | upload the archive, then commit and tag the package |
| `dotnet` | the project `dotnet/{Namespace}/` with `runtimes/<rid>/native/` libraries, and the `.nupkg` that `dotnet pack` builds from it | `dotnet nuget push dist/dotnet/*.nupkg` |
| `kotlin` | the Gradle project `kotlin/{name}/` with prebuilt `jniLibs/<abi>/` (library and shim) and desktop natives under `resources/natives/<platform>/` | `gradle publish` |
| `dart` | the pub package `dart/{package}/` with desktop libraries under `native/<platform>/` | `dart pub publish` |
| `go` | the module `go/{package}/` with libraries under `lib/<platform>/` and cgo flags that link the right one | push the module to its repository |
| `c`, `cpp` | `{library}-{version}-c.tar.gz` and `-cpp.tar.gz`: headers, libraries under `lib/<platform>/`, and a `CMakeLists.txt` exposing the host's library as an imported target | attach to a release |

Wheel tags carry the build's macOS deployment target
(`macosx_11_0_arm64`) and, on Linux, the newest glibc the library actually
links against (`manylinux_2_17_x86_64` or later), so `pip` only installs a
wheel where the library loads. Each archive is deterministic (fixed
timestamps and order), so the same builds always produce the same bytes and
checksums.

A target skips platforms its ecosystem has no slot for (a wheel has no
Android tag, NuGet no `wasm32` runtime), and a target with nothing to ship is
named in a note.

## Platforms

`--platforms` takes a comma-separated list of platform ids; without it,
`build` and `package` use `[build] platforms`, else the host. With
`--binaries`, the default is every platform directory present.

| Platform id | Rust target | Shipped by |
|-------------|-------------|------------|
| `darwin-arm64` | `aarch64-apple-darwin` | every desktop target, and `swift` |
| `darwin-x64` | `x86_64-apple-darwin` | every desktop target, and `swift` |
| `linux-x64` | `x86_64-unknown-linux-gnu` | every desktop target |
| `linux-arm64` | `aarch64-unknown-linux-gnu` | every desktop target |
| `windows-x64` | `x86_64-pc-windows-msvc` | every desktop target |
| `ios-arm64` | `aarch64-apple-ios` | `swift` |
| `ios-sim-arm64` | `aarch64-apple-ios-sim` | `swift` |
| `ios-sim-x64` | `x86_64-apple-ios` | `swift` |
| `android-arm64` | `aarch64-linux-android` | `kotlin` (and `c`) |
| `android-x64` | `x86_64-linux-android` | `kotlin` (and `c`) |
| `wasm32` | `wasm32-unknown-unknown` | `wasm` (and `c`) |

## Swift and the XCFramework

`package` fuses the Apple static libraries into `C{Module}.xcframework`
(iOS device, iOS simulator, and macOS slices, with a group's architectures
fused by `lipo`), zips it, and writes its SHA-256 checksum. The packaged
`Package.swift` declares the C module as
`.binaryTarget(url:checksum:)`, so apps link the library with no flags.
The URL comes from `[generators.swift] xcframework_url`; until you set it,
it's a placeholder under `example.invalid`:

```toml
[generators.swift]
xcframework_url = "https://github.com/acme/kvstore/releases/download/{version}/{file}"
```

To publish, upload the archive to that URL and commit `swift/{Module}/` to
the repository consumers depend on. For local use, unzip the archive next to
the packaged `Package.swift`; the manifest prefers a local
`C{Module}.xcframework` over the URL. This step runs `xcodebuild`, so it
needs macOS.

## What still needs the ecosystem's tools

- **Publishing.** There's no `weaveffi publish`; run each ecosystem's own
  upload command on the artifacts.
- **NuGet.** `package` runs `dotnet pack`, so the .NET SDK must be
  installed; without it the run fails after writing everything else.
- **Kotlin.** The output is a Gradle project, not an `.aar` or `.jar`:
  building one needs the Kotlin compiler, so run `gradle assembleRelease`
  (Android) or `gradle jar` (JVM) on it. With prebuilt shims present, Gradle
  skips the CMake build and needs no NDK.
- **Node.js on Windows.** The addon isn't prebuilt for `windows-x64`, so
  installing there compiles it with node-gyp.
- **Cross-architecture Linux.** Building `linux-arm64` on an x64 host needs a
  cross linker (`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`), and its glue
  isn't prebuilt; use an arm64 runner instead.
- **Dart.** The pub package's loader looks for `native/<platform>/` inside
  the package itself (resolved through its `package:` URI), then relative to
  the working directory, so a compiled app that no longer has the package
  sources copies those libraries next to it. Flutter's native-assets build
  isn't generated.

## CI recipe

Build each platform on a runner that can, upload `target/weaveffi/`, then
package once:

```yaml
jobs:
  build:
    strategy:
      matrix:
        include:
          - runner: macos-latest
            platforms: darwin-arm64,darwin-x64,ios-arm64,ios-sim-arm64
            targets: aarch64-apple-darwin,x86_64-apple-darwin,aarch64-apple-ios,aarch64-apple-ios-sim
          - runner: ubuntu-latest
            platforms: linux-x64,android-arm64,android-x64,wasm32
            targets: aarch64-linux-android,x86_64-linux-android,wasm32-unknown-unknown
          - { runner: ubuntu-24.04-arm, platforms: linux-arm64, targets: "" }
          - { runner: windows-latest, platforms: windows-x64, targets: "" }
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.targets }}
      - uses: actions/setup-node@v4   # for the prebuilt Node.js addon
      - run: cargo install weaveffi-cli
      - run: weaveffi build --platforms ${{ matrix.platforms }}
      - uses: actions/upload-artifact@v4
        with:
          name: weaveffi-${{ matrix.runner }}
          path: target/weaveffi/

  package:
    needs: build
    runs-on: macos-latest   # the XCFramework needs xcodebuild
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with:
          pattern: weaveffi-*
          path: prebuilt
          merge-multiple: true
      - run: cargo install weaveffi-cli
      - run: weaveffi package --binaries prebuilt
```

The Linux runner finds the NDK through `ANDROID_NDK_HOME` (or the SDK
preinstalled on GitHub's images). A platform you can't build can be dropped
from the matrix; every target packages whatever subset is present.
