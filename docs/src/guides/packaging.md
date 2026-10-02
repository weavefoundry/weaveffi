# Packaging

`weaveffi generate` emits binding source. `weaveffi package` goes one step
further: it assembles publishable packages that bundle a prebuilt native
library for each platform, laid out the way each ecosystem resolves native
code, so `pip install`, `npm install`, `dotnet add package`, and friends work
without a local toolchain.

```bash
weaveffi package --binaries prebuilt --target python,node,dotnet -o dist
```

Like `generate`, it reads `[project]` and `[package]` from `weaveffi.toml`
(or takes an explicit input), and every package it writes loads the library
named by the identity's `library` field.

## Where the libraries come from

Pass exactly one source:

- `--binaries <dir>`: prebuilt libraries laid out as
  `<dir>/<platform>/<library file>`. This is the CI path: build each
  platform on its own runner, collect the results, package once.
- `--build <crate>`: cross-compile a Cargo package as a `cdylib` for each
  requested platform. Each platform needs its rustup target and a working
  linker (the Android targets need the NDK's linker); a missing target fails
  with the `rustup target add` command to run.

```text
prebuilt/
  darwin-arm64/libkvstore.dylib
  linux-x64/libkvstore.so
  windows-x64/kvstore.dll
  android-arm64/libkvstore.so
  wasm32/kvstore.wasm
```

## Platforms

`--platforms` takes a comma-separated list of platform ids. With `--build`,
**the default is the host platform only**, so a local run packages what you
just built; CI passes the full list. With `--binaries`, the default is every
platform directory present.

| Platform id | Rust target | Shipped by |
|-------------|-------------|------------|
| `darwin-arm64` | `aarch64-apple-darwin` | every desktop target |
| `darwin-x64` | `x86_64-apple-darwin` | every desktop target |
| `linux-x64` | `x86_64-unknown-linux-gnu` | every desktop target |
| `linux-arm64` | `aarch64-unknown-linux-gnu` | every desktop target |
| `windows-x64` | `x86_64-pc-windows-msvc` | every desktop target |
| `ios-arm64` | `aarch64-apple-ios` | `swift` (static library) |
| `ios-sim-arm64` | `aarch64-apple-ios-sim` | `swift` (static library) |
| `ios-sim-x64` | `x86_64-apple-ios` | `swift` (static library) |
| `android-arm64` | `aarch64-linux-android` | `kotlin` (and `c`) |
| `android-x64` | `x86_64-linux-android` | `kotlin` (and `c`) |
| `wasm32` | `wasm32-unknown-unknown` | `wasm` (and `c`) |

A requested platform with no library is skipped with a warning, and a target
skips platforms its ecosystem has no slot for (a wheel has no wasm tag, NuGet
has no Android runtime). The command prints one line per target it packaged
and a note naming any target that produced nothing.

## What each target produces

| Target | Package layout |
|--------|----------------|
| `c`, `cpp` | Headers plus `lib/<platform>/` libraries and a `CMakeLists.txt` that picks the host's library |
| `swift` | A SwiftPM package with the desktop libraries under `lib/<platform>/`; when an iOS slice is included, `C{Module}.xcframework` (iOS device, iOS simulator, and macOS slices, fused with `lipo`) beside `Package.swift`, which then uses it as a binary target |
| `kotlin` | A Gradle module with Android libraries under `jniLibs/<abi>/` and desktop libraries as classpath resources |
| `node` | A main npm package plus one `optionalDependencies` package per platform, each gated by `os` and `cpu` |
| `wasm` | An npm package with the ES module, its `.d.ts`, and the `.wasm` binary |
| `python` | One platform-wheel tree per platform with the library inside the import package |
| `dotnet` | A NuGet-ready project with libraries under `runtimes/<rid>/native/` |
| `dart` | A pub package with desktop libraries under `native/<platform>/` |
| `go` | A Go module with per-platform libraries and a cgo preamble that links the right one |
| `ruby` | One precompiled platform gem per platform |

Each language page documents its layout in detail. Bundled libraries are
found before the system loader, and `{PREFIX}_LIBRARY` still overrides them.

## Not automated yet

- **Publishing.** There's no `weaveffi publish`; run each ecosystem's own
  publish command on the output.
- **Building wheels and gems.** The Python and Ruby trees are ready to build
  (`python -m build --wheel`, `gem build`), but the CLI doesn't run those
  tools or retag wheels.
- **The Node addon.** The N-API addon is compiled at install time
  (`node-gyp`), so consumers need a C compiler; only the producer library is
  prebuilt.
- **Flutter mobile.** Flutter's native-assets build isn't generated.

## iOS

iOS slices are static libraries, so a Rust producer that ships to iOS
declares `crate-type = ["cdylib", "staticlib"]`. The XCFramework's macOS
slice is the `lib{library}.a` Cargo writes next to the macOS dylib; macOS-only
packages keep linking the dylib. Assembly runs `lipo` and
`xcodebuild -create-xcframework`, so it needs macOS:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
weaveffi package --target swift --build kvstore \
  --platforms ios-arm64,ios-sim-arm64,darwin-arm64 -o dist
```

## CI recipe

Build each platform natively, upload each library under its platform id, then
package once:

```yaml
jobs:
  build:
    strategy:
      matrix:
        include:
          - { platform: darwin-arm64, runner: macos-latest,     target: aarch64-apple-darwin,     lib: libkvstore.dylib }
          - { platform: linux-x64,    runner: ubuntu-latest,    target: x86_64-unknown-linux-gnu, lib: libkvstore.so }
          - { platform: linux-arm64,  runner: ubuntu-24.04-arm, target: aarch64-unknown-linux-gnu, lib: libkvstore.so }
          - { platform: windows-x64,  runner: windows-latest,   target: x86_64-pc-windows-msvc,   lib: kvstore.dll }
          - { platform: wasm32,       runner: ubuntu-latest,    target: wasm32-unknown-unknown,   lib: kvstore.wasm }
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - run: cargo build --release --target ${{ matrix.target }}
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
      - run: >
          weaveffi package --binaries prebuilt -o dist
          --platforms darwin-arm64,linux-x64,linux-arm64,windows-x64,wasm32
```

A platform you can't build can be dropped from the matrix; every target
packages whatever subset is present.
