# FAQ

The top ten questions we hear about WeaveFFI. For broader context see the
[introduction](intro.md), the [comparison page](comparison.md), and the
[per-target generator docs](generators/README.md).

## 1. Why not UniFFI?

[UniFFI](https://mozilla.github.io/uniffi-rs/) is excellent, ships in
production at Mozilla, and is the right choice if you only need Swift,
Kotlin, and Python. We built WeaveFFI because we needed:

- **More targets out of the box.** WeaveFFI ships first-class generators
  for C, C++, Swift, Kotlin/Android, Node.js, Wasm, Python, .NET, Dart,
  Go, and Ruby, eleven in total. UniFFI's first-party language list is
  shorter and the rest live as community extensions of varying maturity.
- **A standalone CLI workflow.** WeaveFFI is a single binary
  (`cargo install weaveffi-cli`) with `validate`, `lint`, `diff`,
  `watch`, `format`, and `extract` subcommands designed to
  drop into CI. UniFFI is a build-script integration first.
- **A non-Rust-only story.** WeaveFFI's IR is language-agnostic: any
  backend that can expose a stable C ABI (Rust, C, C++, Zig, …) can be
  driven from the same IDL. UniFFI is Rust-first.
- **A YAML/JSON/TOML IDL with a JSON Schema.** WeaveFFI ships
  `weaveffi.schema.json` for editor autocompletion. UniFFI's UDL is
  custom-syntax and proc-macro is Rust-only.

If your matrix is only Swift+Kotlin+Python and you want maximum
maturity today, UniFFI is the safer pick. See the
[comparison page](comparison.md) for the full table.

## 2. Can I use it with C++ codebases?

Two distinct cases:

- **Generating C++ bindings for consumers.** Yes, `--target cpp`
  emits a header-only RAII C++ API (`weaveffi.hpp`) with
  `std::optional`, `std::vector`, `std::unordered_map`, exception-based
  errors, move semantics, and a `CMakeLists.txt`. See the
  [C++ generator docs](generators/cpp.md).
- **Wrapping an existing C++ library.** WeaveFFI does not parse C++
  headers; you describe the surface area you want to expose in the
  IDL and the C++ implementation provides the stable C ABI symbols.
  If you want to start from C++ headers and auto-generate, look at
  [autocxx](https://github.com/google/autocxx) or
  [SWIG](https://www.swig.org/).

## 3. Does it support generics?

Yes, with a curated set of built-in generic shapes rather than open
user-defined generics:

- `handle<T>`: typed opaque pointers (compile-time-checked handle
  types per resource).
- `iter<T>`: lazy streaming sequences with `_next` / `_destroy` ABI.
- `[T]`: homogeneous lists.
- `{K:V}`: homogeneous maps (passed as parallel key/value arrays at
  the C ABI).
- `T?`: optionals.
- `&str`, `&[u8]`: borrowed views (no copy at the boundary).

We deliberately do **not** support arbitrary user-defined generics
(e.g. `Result<MyType, MyError>` parameterized at the IDL level).
Cross-language generic monomorphization is a rabbit hole; the
built-in shapes cover ~95% of real-world FFI surface area without
requiring every target generator to implement type-erasure logic.

## 4. What's the runtime overhead?

WeaveFFI itself adds **no runtime** beyond the small `weaveffi-abi`
crate (a few hundred lines: error helpers, string/byte-slice
allocators, cancel tokens). Per-call overhead is the cost of:

1. Marshalling arguments across the C ABI (string→`const char*`,
   list→`*ptr + len`, etc.). Borrowed types (`&str`, `&[u8]`) avoid
   copies.
2. The single `extern "C"` function call.
3. Marshalling the return value back.

For primitive arguments and return types, this is roughly the cost of
a normal function call plus an out-pointer write for the error. For
larger structs, lists, and maps, it's dominated by the underlying
allocation/copy cost, not by anything WeaveFFI inserts.

Async functions add a callback indirection (the C ABI is callback-based)
plus whatever runtime your backend uses. There is no scheduler imposed
by WeaveFFI; the implementation chooses how to spawn work.

## 5. How are errors propagated?

Every generated function takes a trailing `weaveffi_error* out_err`
parameter. On success the runtime sets `code = 0` and
`message = NULL`. On failure it sets a non-zero code and a
heap-allocated UTF-8 message that the caller frees via
`weaveffi_error_clear`.

Each target language maps this to its native error story:

- **C**: direct `weaveffi_error` struct.
- **C++**: exceptions (`WeaveFFIError` + per-code subclasses).
- **Swift**: `throws` + `WeaveFFIError`.
- **Kotlin**: checked exceptions (`WeaveFFIException`).
- **Node.js / TypeScript**: thrown `Error` objects (or
  `Promise.reject` for `async`).
- **Wasm/JS**: thrown `Error`.
- **Python**: raised `WeaveFFIError`.
- **.NET**: thrown `WeaveFFIException`.
- **Dart**: thrown `WeaveFFIException`.
- **Go**: second `error` return value.
- **Ruby**: raised `WeaveFFIError`.

You can also declare named error domains in the IDL (per module) to
assign stable numeric codes to expected failures. See the
[Error Handling guide](guides/errors.md).

## 6. Can I customize the generated code?

Yes, via two escape hatches in increasing order of power:

1. **Generator config** (`--config cfg.toml` or inline `generators:`
   table in the IDL). Controls Swift module names, Android package,
   C prefix, C++ namespace, Dart/Go/Ruby package names, and other
   per-target knobs. See the
   [Generator Configuration guide](guides/config.md).
2. **Hook commands** (`pre_generate` / `post_generate` in the
   config). Run arbitrary shell commands before and after generation,
   useful for `prettier`, `swiftformat`, `gofmt`, etc.

If you need to change the C ABI shape itself, that's a generator
contribution. See [`CONTRIBUTING.md`](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md#adding-a-new-generator).

## 7. Does it work with Flutter?

Yes, `--target dart` emits `dart:ffi` bindings plus a `pubspec.yaml`
that's drop-in compatible with both Flutter and pure Dart projects.
You ship the generated package alongside the `cdylib` for each
platform Flutter targets (iOS framework, Android `.so` per ABI, macOS
`.dylib`, Linux `.so`, Windows `.dll`).

The generated Dart code uses the standard `package:ffi` helpers, so
it works on every Flutter platform that supports `dart:ffi` (i.e.
everything except Web today; for the browser, use `--target wasm`
and load the bindings via JS interop). See the
[Dart generator docs](generators/dart.md).

## 8. Is it Windows-friendly?

Yes, WeaveFFI itself builds and runs on Windows (the CLI is plain
Rust, no platform-specific dependencies). Generated outputs target
Windows correctly:

- **C / C++**: emitted headers are compiler-agnostic (MSVC, clang,
  gcc), and every prototype carries a portable `WEAVEFFI_API`
  visibility macro. Consumers resolve it to `__declspec(dllimport)`;
  a C/C++/Zig backend that implements the header builds its library
  with `WEAVEFFI_BUILD` defined to export the symbols via
  `__declspec(dllexport)` (see the
  [C generator docs](generators/c.md#symbol-visibility)).
- **.NET**: P/Invoke uses `DllImport` with the right calling
  conventions and looks up `weaveffi.dll`.
- **Node.js**: the N-API addon builds with `node-gyp` on Windows.
- **Python**: `ctypes` loads `weaveffi.dll`.
- **Dart**: looks up `weaveffi.dll` via `Platform.isWindows`.
- **Go / Ruby**: load the appropriate Windows shared library.

CI runs the Python end-to-end consumer test on Windows on every PR
to keep the platform honest. The other targets are exercised on macOS
and Linux only. If you hit a Windows-specific issue, please open an
issue.

## 9. How do I distribute the cdylib?

You build a platform-specific shared library per target triple and
ship it alongside the generated package. Three common patterns:

- **Per-platform npm/PyPI/gem packages.** Publish one package per
  `(os, arch)` and use a small loader in the consumer that picks the
  right binary at install or runtime. WeaveFFI generates the
  TypeScript/Python/Ruby loader, you supply the binaries.
- **`xcframework` for Swift.** Bundle iOS device, iOS simulator,
  and macOS slices into a single `.xcframework` that SwiftPM can
  consume. The generated `Package.swift` references it as a
  `.binaryTarget`.
- **`.aar` for Android.** Package the JNI shim + per-ABI `.so` files
  into an Android Archive that Gradle resolves like any other
  dependency. The generated `build.gradle` skeleton is compatible
  with this layout.

The name, version, and metadata stamped into every generated manifest
(`package.json`, `pyproject.toml`, `*.gemspec`, `*.csproj`, `pubspec.yaml`,
`Package.swift`, `go.mod`, ...) come from a single
[`package:` block](reference/idl.md#package-metadata) in your IDL, so you set
your identity once and every ecosystem stays in sync.

There is no opinionated "weaveffi publish" command today; you use
each ecosystem's normal publish flow. The
[generator-specific docs](generators/README.md) cover the recommended
build matrix per language.

## 10. What's the licensing?

WeaveFFI is dual-licensed under
[MIT](https://github.com/weavefoundry/weaveffi/blob/main/LICENSE-MIT) **OR**
[Apache-2.0](https://github.com/weavefoundry/weaveffi/blob/main/LICENSE-APACHE)
at your option, the same dual-license used by the Rust project itself.

You can use WeaveFFI in commercial, closed-source, or open-source
projects without restriction. Generated code carries no license header
of its own; it's yours to license however you like. Contributions
to the WeaveFFI repo are accepted under the same MIT-or-Apache-2.0
dual license; see [`CONTRIBUTING.md`](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md#license).
