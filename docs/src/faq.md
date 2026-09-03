# FAQ

The questions we hear most about WeaveFFI. For broader context see the
[introduction](intro.md), the [comparison page](comparison.md), the
[C ABI contract](reference/abi.md), and the
[per-target generator docs](generators/README.md).

## 1. Why not UniFFI?

[UniFFI](https://mozilla.github.io/uniffi-rs/) is excellent, ships in
production at Mozilla, and is the right choice if you only need Swift,
Kotlin, and Python. We built WeaveFFI because we needed:

- **More targets out of the box.** WeaveFFI ships first-class generators
  for C, C++, Swift, Kotlin, Node.js, Wasm, Python, .NET, Dart, Go, and
  Ruby, eleven in total, all consuming one C ABI. UniFFI's first-party
  language list is shorter and the rest live as external bindgens of varying
  maturity.
- **A standalone CLI workflow.** WeaveFFI is a single binary
  (`cargo install weaveffi-cli`) with `generate`, `validate`, `diff`,
  `extract`, and `package` subcommands designed to drop into CI. UniFFI is a
  build-script integration first.
- **A non-Rust-only story.** WeaveFFI's IR is language-agnostic: any
  backend that can expose a stable C ABI (Rust, C, C++, Zig, ...) can be
  driven from the same IDL and implement the same generated header. UniFFI
  is Rust-first and its scaffolding ABI is private.
- **A YAML/JSON/TOML IDL with a JSON Schema.** WeaveFFI ships
  `weaveffi.schema.json` for editor autocompletion. UniFFI's UDL is
  custom-syntax and its proc-macro path is Rust-only.

Since ABI revision 2 the object model is comparable: WeaveFFI interfaces are
reference-counted `Arc<T>` objects that can be shared and nested inside
records and collections, and callback interfaces are traits the consumer
implements. UniFFI is still ahead on callback methods that return strings,
records, or objects, on async callback methods, and on cross-crate type
imports (see the [roadmap](roadmap.md)). If your matrix is only
Swift+Kotlin+Python and you want maximum maturity today, UniFFI is the safer
pick. See the [comparison page](comparison.md) for the full table.

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

- `iter<T>`: lazy streaming sequences with a `_next` / `_destroy` ABI.
- `[T]`: homogeneous lists.
- `{K:V}`: homogeneous maps.
- `T?`: optionals.

All four compose with each other and with every other type, including
interface objects: `[Store]`, `{string:Store}`, `Store?`, and `iter<Store>`
are all valid, and records may carry any of them as fields. Lists, maps,
optionals, records, and rich enums cross the C ABI serialized as
[value buffers](reference/value-buffers.md); objects inside them travel as
tokens that carry one strong reference.

We deliberately do **not** support arbitrary user-defined generics (for
example `Result<MyType, MyError>` parameterized at the IDL level) or traits
with generic methods. Cross-language generic monomorphization is a rabbit
hole; the built-in shapes cover the large majority of real-world FFI surface
area without requiring every target generator to implement type-erasure
logic. Trait-object interfaces (one declared method set, several producer
implementations) are on the [roadmap](roadmap.md).

## 4. What's the runtime overhead?

WeaveFFI itself adds **no runtime** beyond the small `weaveffi-abi`
crate (error helpers, string and byte-buffer allocators, the value-buffer
codec, the object reference-count shims, the callback vtable shims, cancel
tokens, and the async spawner hook). Per-call overhead is the cost of:

1. Marshalling arguments across the C ABI (`string` to `const char*`,
   `bytes` to `ptr + len`, and so on). String and byte parameters are
   borrowed views for the duration of the call, so the producer copies only
   what it keeps.
2. The single `extern "C"` function call.
3. Marshalling the return value back.

For primitive arguments and return types, this is roughly the cost of
a normal function call plus an out-pointer write for the error. Passing an
object is one pointer; the producer clones the `Arc` only if it retains it.
For larger structs, lists, and maps, it's dominated by the encode/decode of
the value buffer, which is a single allocation per value on each side.

Async functions add a callback indirection (the C ABI is callback-based)
plus whatever executor drives the future. The default spawner runs each
future on a dedicated thread; producers that already have a runtime plug it
in with `weaveffi::set_spawner` (see question 13).

## 5. How are errors propagated?

At the C ABI, generated functions take a trailing `weaveffi_error*
out_err` parameter. On success the runtime sets `code = 0` and
`message = NULL`. On failure it sets a non-zero code and a
heap-allocated UTF-8 message that the caller frees via
`weaveffi_error_clear`. Negative codes are reserved for the runtime:
`-1` generic, `-2` producer panic, `-3` marshalling failure, and `-4`
a consumer callback-interface implementation raised.

Above the ABI, error surfacing is opt-in per function: a module
declares an error domain (`errors:` in the IDL) with named, stable
codes, and functions marked `throws: true` surface those codes as the
domain's typed error in each language. Taking a `KvError` domain as
the example:

- **C**: direct `weaveffi_error` struct, plus an enum constant per
  code (`weaveffi_kv_KvError_KeyNotFound`).
- **C++**: per-domain exception types (`KvError` + per-code
  subclasses).
- **Swift**: `throws` with a domain enum conforming to `Error`
  (`catch KvError.keyNotFound`).
- **Kotlin**: domain exceptions (`KvException` extends
  `WeaveFFIException`, one nested class per code).
- **Node.js / TypeScript**: domain error classes (`KvError` extends
  `WeaveFFIError`); async functions reject the promise with them.
- **Wasm/JS**: the same domain error classes.
- **Python**: a domain exception hierarchy (`KvError` extends
  `WeaveFFIError`, `KeyNotFound` extends `KvError`).
- **.NET**: thrown `KvException` (extends `WeaveFFIException`).
- **Dart**: thrown `KvException` (extends `WeaveFFIException`).
- **Go**: a second `error` return carrying a typed error struct
  matched via `errors.As` and code constants
  (`KvErrorKeyNotFound`).
- **Ruby**: domain exception classes (`KvError` with nested
  per-code classes).

Non-throwing functions (the default) return plain values; a non-zero
code on one of them only ever reports a producer bug, so the wrapper
panics or traps instead of surfacing an error type. See the
[Error Handling guide](guides/errors.md).

## 6. Can I customize the generated code?

Yes, via two escape hatches in increasing order of power:

1. **Project config** (`[generators.<target>]` tables in the
   `weaveffi.toml` next to your definition). Controls Swift module
   names, the Kotlin package, C prefix, C++ namespace, Dart/Go/Ruby package
   names, module-prefix stripping (`strip_module_prefix`), Wasm's
   Emscripten mode, and other per-target knobs. See the
   [Project Configuration guide](guides/config.md).
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

CI builds and tests the workspace on Windows on every PR, and a
dedicated Windows job generates every target's bindings and verifies
the output. The full conformance harness (running the generated
bindings in all eleven languages) is exercised on Linux. If you hit a
Windows-specific issue, please open an issue.

## 9. How do I distribute the cdylib?

You build a platform-specific shared library per target triple and
ship it alongside the generated package. `weaveffi package` does the
assembly: point it at prebuilt libraries with `--binaries <dir>` (or let it
cross-compile a Rust producer with `--build <crate>`) and it lays each
target's package out the way its ecosystem expects, including
`jniLibs/<abi>/` for Kotlin and the `.wasm` binary inside an npm package for
Wasm (which needs a `wasm32` build). Three common layouts:

- **Per-platform npm/PyPI/gem packages.** Publish one package per
  `(os, arch)` and let the generated loader pick the right binary at
  runtime. WeaveFFI generates the TypeScript/Python/Ruby loader, you supply
  the binaries.
- **`xcframework` for Swift.** Bundle iOS device, iOS simulator,
  and macOS slices into a single `.xcframework` that SwiftPM can
  consume. The generated `Package.swift` references it as a
  `.binaryTarget`.
- **`.aar` for Android.** Package the JNI shim plus per-ABI `.so` files
  (`android-arm64`, `android-x64`) into an Android Archive that Gradle
  resolves like any other dependency. The generated `build.gradle.kts` is
  compatible with this layout, and the same wrapper also runs on the desktop
  JVM.

The name, version, and metadata stamped into every generated manifest
(`package.json`, `pyproject.toml`, `*.gemspec`, `*.csproj`, `pubspec.yaml`,
`Package.swift`, `go.mod`, ...) come from the single
[`[package]` table](guides/config.md#package) of your `weaveffi.toml`, so you
set your identity once and every ecosystem stays in sync.

There is no "weaveffi publish" command; you use each ecosystem's normal
publish flow on the output of `weaveffi package`. See the
[Packaging and Distribution guide](guides/packaging.md) and the
[generator-specific docs](generators/README.md) for the recommended build
matrix per language.

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

## 11. Who owns an object, and when is it freed?

The producer does. Every interface object is reference counted on the native
side (an `Arc<T>` in a Rust producer), and the C ABI exposes two symbols per
interface: `{tag}_clone`, which returns a new strong reference to the same
object, and `{tag}_destroy`, which releases one. The object is dropped when
the last reference goes away, wherever that reference lives.

Each generated wrapper holds exactly one strong reference and releases it
through the language's natural disposal hook: `deinit` in Swift, the
destructor in C++ (copying a wrapper calls `_clone`), `close()` or a `using`
declaration in Node.js and Wasm, `close()` or a `with` block in Python,
`Dispose()` in .NET, `close()` (`AutoCloseable`) in Kotlin, `dispose()` in
Dart, `Close()` in Go, and `close` in Ruby. Every managed target also
registers a garbage-collector backstop (`Cleaner`, `FinalizationRegistry`,
`__del__`, a finalizer, `NativeFinalizer`, `runtime.SetFinalizer`,
`FFI::AutoPointer`) so a wrapper that's never closed still releases its
reference eventually. Closing a wrapper twice is a no-op; using one after
closing it throws.

Because the count lives in the producer, ownership is uniform in every
position: an object returned from a call, produced by an iterator, delivered
by an async completion, or passed to a callback-interface method is one
reference the consumer adopts. An object passed *to* the producer as a
parameter is borrowed for the call, and the producer clones it if it keeps
it. An object inside a record, list, map, or optional travels as a token
carrying one reference, so two wrappers (or a wrapper and a producer-side
`Arc`) can point at the same object and neither can free it out from under
the other. See [Memory Ownership](guides/memory.md) and the
[C ABI contract](reference/abi.md#objects-interfaces).

## 12. How do callback interfaces work?

A callback interface is a set of methods the consumer implements and the
producer calls. In the IDL it's a `callback_interfaces:` entry with
`methods:`; in Rust it's

```rust
#[weaveffi::callback_interface]
pub trait EvictionListener: Send + Sync {
    fn on_evict(&self, entry: &Entry, reason: EvictionReason) -> bool;
}
```

accepted by any function, constructor, static, or method as
`Arc<dyn EvictionListener>`. Each target exposes it as the natural thing to
implement: a Swift `protocol`, a Kotlin, Go, or C# interface, a Python `ABC`,
a Dart `abstract class`, a C++ class with virtual methods passed as a
`std::shared_ptr`, a TypeScript `interface` in Node.js and Wasm, and a Ruby
module. At the C ABI it's a `void* ctx` plus a static vtable of function
pointers with a trailing `free(ctx)`; the producer may call the methods any
number of times from any thread and calls `free` exactly once when it drops
its last reference. There is no unregister call to forget.

Methods are synchronous, never `throws` or `async`, and return nothing, a
scalar, `bool`, or a C-style enum; parameters may be anything except another
callback interface or an iterator, and an object parameter hands the consumer
a reference it adopts. If the consumer's implementation raises, the wrapper
reports it through the method's `out_err` slot with `FOREIGN_ERROR_CODE`
(`-4`); the producer aborts the call it was making (in Rust, the call
unwinds) and the original caller sees that code and message instead of a
crash. Producers should therefore not hold a `Mutex` guard across a callback
call. Richer return types and async callback methods are on the
[roadmap](roadmap.md). The `events` and `kvstore` [samples](samples.md) show
both patterns.

## 13. Which executor runs my async functions?

Whichever you install. An `async` export lowers to a launcher that returns
immediately and a completion callback that fires exactly once, from a
producer thread. Something has to drive the future in between, and the
`weaveffi-abi` runtime has a pluggable `Spawner` hook for that. The default
spawner needs no runtime: it detaches a thread per future and blocks on it,
which is fine for CPU-bound work and for futures woken from other threads.

A producer whose futures need a reactor (Tokio's I/O or timers, for example)
calls `weaveffi::set_spawner` once at startup:

```rust
let handle = tokio::runtime::Handle::current();
weaveffi::set_spawner(move |fut| {
    handle.spawn(fut);
})
.expect("spawner installed once");
```

The first call wins; a second returns `SpawnerAlreadySet`. Every future handed
to a spawner is already wrapped so a panic inside it is caught and reported
through the completion callback as `PANIC_ERROR_CODE`, so a spawner never
sees an unwinding future. On `wasm32`, which has no threads, the default
spawner drives the future inline before the launcher returns. Consumers don't
see any of this: they get `async/await`, `Promise`, `suspend`, `Task<T>`, or
`Future<T>` as usual. See the [Async Functions guide](guides/async.md).
