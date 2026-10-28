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
needs, so the crate's own `crate-type` doesn't matter. It builds with the
`release` profile unless `--profile` (or `[build] profile`) names another,
and with the project's Cargo features (`-F`/`--features`, `--all-features`,
`--no-default-features`, or the `[build]` keys). A Rust producer's API is
then read from the first library it built (see [Library Mode](extract.md)),
so nothing is compiled twice. `build` produces only the producer's library:
no target needs compiled glue of its own.

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
  darwin-arm64/  libkvstore.dylib  libkvstore.a
  ios-arm64/     libkvstore.a
  android-arm64/ libkvstore.so
  wasm32/        kvstore.wasm
```

Every platform is checked before anything compiles; a platform this machine
can't build (a missing rustup target, a missing NDK, or a host that can't
build it: Apple platforms need macOS, Windows needs Windows) is skipped with
a warning naming the fix, and the run fails only when no requested platform
can be built.

`build` and `package` follow one policy for a missing external tool (a C
compiler, the NDK, Xcode, the .NET SDK): they warn, skip the artifact that
needs it, finish everything else, and list every skipped artifact at the
end. The run still exits `0`; pass `--strict` to exit `1` when anything was
skipped (as CI should).

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
| `python` | one platform wheel per desktop platform, the library inside the import package | `twine upload` |
| `node` | one npm tarball per desktop platform (`{package}-{os}-{cpu}`) carrying only the library, plus the main package, which lists them as optional dependencies and depends on koffi | `npm publish` for each, platform packages first |
| `wasm` | an npm tarball with the `.wasm` module | `npm publish` |
| `ruby` | one platform gem per desktop platform | `gem push` |
| `swift` | `C{Module}.xcframework.zip` (static libraries for every Apple platform built), its `.sha256`, and the SwiftPM package whose binary target points at the archive's URL and checksum | upload the archive, then commit and tag the package |
| `dotnet` | a project with `runtimes/<rid>/native/` libraries, and the `.nupkg` that `dotnet pack` builds from it | `dotnet nuget push` |
| `kotlin` | a Gradle project with the Android libraries under `src/main/jniLibs/<abi>/` and the desktop ones under JNA's `src/main/resources/<os>-<arch>/` | Gradle |
| `dart` | a pub package with the desktop libraries under `native/<platform>/` | `dart pub publish` |
| `go` | a module with the libraries and cgo flags that link the right one | push the module to its repository |
| `c`, `cpp` | archives of the headers, the libraries per platform, and a `CMakeLists.txt` | attach to a release |

Each target's page under [Generators](../generators/README.md) documents its
artifacts' names and layout. Each archive is deterministic (fixed timestamps
and order), so the same builds always produce the same bytes and checksums.
A target skips platforms its ecosystem has no slot for (a wheel has no
Android tag, NuGet no `wasm32` runtime), and a target with nothing to ship
is named in a note.

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
`Package.swift` declares the C module as a `.binaryTarget(url:checksum:)`,
so apps link the library with no flags. The URL comes from the Swift
target's configuration (see [Swift](../generators/swift.md)); to publish,
upload the archive to that URL and commit the package to the repository
consumers depend on. This step runs `xcodebuild`, so it needs macOS with
Xcode; elsewhere the Swift package is skipped.

## What still needs the ecosystem's tools

- **Publishing.** There's no `weaveffi publish`; run each ecosystem's own
  upload command on the artifacts.
- **NuGet.** `package` runs `dotnet pack`, so the .NET SDK must be
  installed; without it the `.nupkg` is skipped (and `--strict` fails the
  run) after everything else is written.
- **Kotlin.** The output is a Gradle project, not an `.aar` or `.jar`, so
  build one with Gradle.
- **Cross-architecture Linux.** Building `linux-arm64` on an x64 host needs a
  cross linker (`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`); an arm64
  runner is simpler.

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
      - run: cargo install weaveffi-cli
      - run: weaveffi build --platforms ${{ matrix.platforms }} --strict
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
      - run: weaveffi package --binaries prebuilt --strict
```

The Linux runner finds the NDK through `ANDROID_NDK_HOME` (or the SDK
preinstalled on GitHub's images). A platform you can't build can be dropped
from the matrix; every target packages whatever subset is present.
