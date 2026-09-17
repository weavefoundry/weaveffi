# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.9.0-orange)](./weaveffi.schema.json) [![downloads](https://img.shields.io/crates/d/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli)

WeaveFFI generates type-safe bindings for 11 languages for any native library
that exposes a C ABI, whether it's written in Rust, C, C++, Zig, or anything
else: no hand-written JNI, no duplicate implementations, no unsafe boilerplate.
Define your API once as an IDL in YAML, JSON, or TOML and ship idiomatic
packages for C, C++, Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart,
Go, and Ruby that all talk to the same stable C ABI. Interfaces become real,
reference-counted objects that consumers can share, store in records and
lists, and release deterministically; callback interfaces let the native
library call back into code the consumer wrote; and error domains become typed
errors consumers can catch and match on, not flat functions and raw integer
codes. Writing your producer in Rust? Annotate a normal module with
`#[weaveffi::module]` and the macro generates both the C ABI and the IDL for
you. Every path shares one engine, so the library you build and the bindings
you ship cannot drift.

## Quickstart

**1. Install the CLI:**

```bash
cargo install weaveffi-cli
```

**2. Define your API as an IDL** in `kvstore.yml`. Any native library that
exposes a C ABI (written in C, C++, Zig, Rust, ...) implements the symbols it
declares. An interface is a reference-counted object with constructors and
methods; an error domain plus `throws: true` gives its fallible members typed
errors:

```yaml
version: "0.9.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - { name: KeyNotFound, code: 1001, message: "key not found" }
        - { name: StoreFull, code: 1003, message: "store has reached capacity" }
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
            throws: true
        methods:
          - name: put
            params:
              - { name: key, type: string }
              - { name: value, type: bytes }
            return: bool
            throws: true
          - name: count
            params: []
            return: i64
```

**Producing in Rust?** Skip the hand-written IDL: annotate a normal module with
`#[weaveffi::module]` (after `cargo add weaveffi`) and the macro emits the C ABI
and derives the IDL for you, so you write no `unsafe` glue. An interface type
must be `Send + Sync` (the object is shared across the boundary and reference
counted as an `Arc<T>`); its `pub` methods take `&self` or `self: Arc<Self>`.
See [The Rust Producer Macro](docs/src/guides/producer-macro.md) for the full
walkthrough.

```rust
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[weaveffi::error]
    #[derive(Debug)]
    pub enum KvError {
        /// key not found
        KeyNotFound = 1001,
        /// store has reached capacity
        StoreFull = 1003,
    }

    // Reference counted by the producer; every consumer wrapper holds one
    // strong reference and releases it when it's closed or collected.
    #[weaveffi::interface]
    pub struct Store {
        entries: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl Store {
        pub fn open(path: String) -> Result<Store, KvError> {
            let _ = path; // in-memory demo
            Ok(Store { entries: Mutex::new(BTreeMap::new()) })
        }

        pub fn put(&self, key: String, value: Vec<u8>) -> Result<bool, KvError> {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= 1024 && !entries.contains_key(&key) {
                return Err(KvError::StoreFull);
            }
            Ok(entries.insert(key, value).is_none())
        }

        pub fn count(&self) -> i64 {
            self.entries.lock().unwrap().len() as i64
        }
    }
}

// Emit the fixed C ABI runtime surface once per cdylib.
weaveffi::export_runtime!();
```

**3. Generate bindings** from the IDL (or, for a Rust producer, straight from
the annotated source):

```bash
weaveffi generate kvstore.yml -o generated --target c,swift,python
# Rust producer: point generate at the annotated source instead
weaveffi generate src/lib.rs  -o generated --target c,swift,python
```

**4. Use the generated code from any of the eleven supported languages.**
Every target gets a real `Store` class whose objects hold one strong
reference to the native store and release it deterministically (`deinit`,
`close()`, `Dispose()`, RAII) with a garbage-collector backstop where the
language has one, plus a typed `KvError` consumers can catch and match on.
Click each block below to see what WeaveFFI emits.

<details>
<summary><strong>C</strong>: <code>generated/c/weaveffi.h</code></summary>

```c
typedef enum { weaveffi_kv_KvError_KeyNotFound = 1001, weaveffi_kv_KvError_StoreFull = 1003 } weaveffi_kv_KvError;
typedef struct weaveffi_kv_Store weaveffi_kv_Store;

// Module: kv
WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_open(const char* path, weaveffi_error* out_err);
WEAVEFFI_API bool weaveffi_kv_Store_put(const weaveffi_kv_Store* self, const char* key, const uint8_t* value_ptr, size_t value_len, weaveffi_error* out_err);
WEAVEFFI_API int64_t weaveffi_kv_Store_count(const weaveffi_kv_Store* self, weaveffi_error* out_err);
/** Returns a new strong reference to the same object (the pointer value is unchanged). Null is a no-op returning null. */
WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_clone(const weaveffi_kv_Store* self);
/** Releases one strong reference; the object is dropped when the last reference is released. Null is a no-op. */
WEAVEFFI_API void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
```

</details>

<details>
<summary><strong>Swift</strong>: <code>generated/swift/Sources/WeaveFFI/WeaveFFI.swift</code></summary>

```swift
public enum KvError: Error, LocalizedError {
    case keyNotFound(message: String)
    case storeFull(message: String)
}

public final class Store {
    let ptr: OpaquePointer

    deinit {
        weaveffi_kv_Store_destroy(ptr)
    }

    public static func open(path: String) throws -> Store { /* ... */ }

    public func put(key: String, value: Data) throws -> Bool { /* ... */ }

    public func count() -> Int64 { /* ... */ }
}
```

</details>

<details>
<summary><strong>Python</strong>: <code>generated/python/kvstore/weaveffi.pyi</code></summary>

```python
class KvError(WeaveFFIError):
    KeyNotFound: Type["KeyNotFound"]
    StoreFull: Type["StoreFull"]

class KeyNotFound(KvError):
    CODE: int  # 1001

class StoreFull(KvError):
    CODE: int  # 1003

class Store:
    def close(self) -> None: ...
    def __enter__(self) -> "Store": ...
    def __exit__(self, *exc: object) -> bool: ...
    @classmethod
    def open(cls, path: str) -> "Store": ...
    def put(self, key: str, value: bytes) -> bool: ...
    def count(self) -> int: ...
```

</details>

The remaining targets follow the same pattern with their own idioms: a class
(or the closest analogue) that owns one reference to the native object and
releases it through the language's natural disposal hook, and the module's
error domain as a typed error or exception. Add a `callback_interfaces:` block
to the IDL and each target also gets an interface, protocol, or abstract class
the consumer implements and the native library calls.

## Why WeaveFFI?

- **One definition, eleven languages.** Write the API once (safe Rust or an
  IDL) and ship packages to npm, SwiftPM, Maven, PyPI, NuGet, pub.dev,
  RubyGems, and Go modules. Each package is standalone: consumers don't need
  WeaveFFI installed.
- **Stable C ABI underneath.** Every target speaks to the same `extern "C"`
  contract, so adding a new platform later is a code-gen change, not a
  rewrite. Rust producers get that C ABI for free from the
  `#[weaveffi::module]` macro; any other backend that can expose a C ABI (C,
  C++, Zig) implements the generated header directly.
- **Idiomatic per-target output.** No lowest-common-denominator surface area.
  Interfaces become real classes with methods and deterministic disposal, and
  error domains become typed errors (a Swift error enum, Python exception
  classes, and each remaining target's own exception idiom). Swift gets
  `async/await` and `throws`, Kotlin gets `suspend` and JNI glue, Python
  gets typed `.pyi` stubs, TypeScript gets `Promise`s and `BigInt`, and Dart
  gets `dart:ffi` with `NativeFinalizer`, all from the same definition.
- **Objects that compose.** Interface objects are reference counted by the
  producer (`Arc<T>` in Rust) and expose `_clone`/`_destroy` at the C ABI, so
  they can be returned, passed, shared between wrappers, and nested inside
  records, optionals, lists, maps, iterators, and async results. Every wrapper
  releases its reference when it's closed, disposed, or dropped.
- **Callback interfaces.** Declare a `callback_interfaces:` block (or a
  `#[weaveffi::callback_interface] trait` in Rust) and the consumer implements
  it as a protocol, interface, or abstract class. The producer receives an
  `Arc<dyn Trait>` it can retain and call from any thread; a consumer-side
  failure surfaces to the original caller as a foreign error rather than a
  crash.
- **The whole IDL surface, on every target.** Interfaces, typed error
  domains, async functions (with a pluggable spawner, so Tokio users can plug
  in their runtime), iterators, and callback interfaces work across all
  eleven languages. Generators declare their capabilities and
  `weaveffi generate` fails with a clear error (never a silent skip) in the
  rare mode that can't deliver a feature you use (Wasm's Emscripten compat
  mode excludes async functions and callback interfaces). See the
  [feature matrix](docs/src/generators/README.md#feature-support-matrix).

## How does it compare?

See [Comparison](docs/src/comparison.md) for a side-by-side feature matrix
versus UniFFI, Diplomat, cbindgen, swift-bridge, napi-rs, wasm-bindgen, SWIG,
and autocxx, plus an honest "when to choose WeaveFFI" guide.

## Supported targets

Every target consumes the same C ABI (revision 2). The table lists what each
generator emits, how its wrapper owns an interface object (each wrapper holds
one strong reference and releases it through the listed hook), and how a
consumer implements a callback interface.

| Target | Output directory | What you get | Objects | Callback interfaces |
|--------|------------------|--------------|---------|---------------------|
| **C** | `c/` | `weaveffi.h` header with opaque interface typedefs, `_clone`/`_destroy` pairs, vtable typedefs, function prototypes, and the shared `weaveffi_error` type | Raw `{tag}*`; call `_clone` to share and `_destroy` to release | Fill a `{tag}_vtable` struct (`ctx` + function pointers + `free`) by hand |
| **C++** | `cpp/` | RAII header (`weaveffi.hpp`) with `std::optional`/`std::vector`/`std::unordered_map` wrappers, exception-based errors, and a `CMakeLists.txt` | Copyable RAII class (copy calls `_clone`, destructor calls `_destroy`) | Subclass the abstract class and pass a `std::shared_ptr<T>` |
| **Swift** | `swift/` | SwiftPM package wrapping the C ABI with `throws`, `async/await`, and `Sequence` iterators | `final class` releasing in `deinit` | Conform to the generated `protocol` |
| **Kotlin** | `kotlin/` | Kotlin wrapper, JNI C shim, and a Gradle (`build.gradle.kts`) project for Android or the desktop JVM | `AutoCloseable` class with a `java.lang.ref.Cleaner` backstop | Implement the generated `interface` |
| **Node.js** | `node/` | N-API addon loader + TypeScript declarations (`BigInt` for 64-bit integers) and a `package.json` | Class with `close()` / `[Symbol.dispose]` and a `FinalizationRegistry` backstop | Pass any object matching the TypeScript `interface` |
| **Wasm** | `wasm/` | JavaScript loader + TypeScript declarations for `wasm32-unknown-unknown` builds, packaged as npm | Class with `close()` / `[Symbol.dispose]` and a `FinalizationRegistry` backstop | Pass any object matching the TypeScript `interface`; delivery is synchronous |
| **Python** | `python/` | `ctypes` bindings + `.pyi` type stubs + `pyproject.toml` | Class with `close()`, context-manager support, and a `__del__` backstop | Subclass the generated `ABC` |
| **.NET** | `dotnet/` | C# P/Invoke bindings + `.csproj` + `.nuspec` for NuGet | `IDisposable` class with a finalizer backstop | Implement the generated `I*` interface |
| **Dart** | `dart/` | `dart:ffi` bindings + `pubspec.yaml` for Flutter and Dart projects | `Finalizable` class with `dispose()` and a `NativeFinalizer` backstop | Extend the generated `abstract class` |
| **Go** | `go/` | CGo bindings + `go.mod` for Go modules | Struct with `Close()` and a `runtime.SetFinalizer` backstop | Implement the generated `interface` |
| **Ruby** | `ruby/` | FFI gem bindings + `.gemspec` for RubyGems | Class with `close` backed by an `FFI::AutoPointer` | Include the generated module and implement its methods |

## Install

**From crates.io** (requires the [Rust toolchain](https://rustup.rs/)):

```bash
cargo install weaveffi-cli
```

**Pre-built binaries** for macOS, Linux, and Windows are attached to every
[GitHub release](https://github.com/weavefoundry/weaveffi/releases). Download
the archive for your platform, extract the `weaveffi` binary, and put it on
your `PATH`.

Verify the install:

```bash
weaveffi --version
weaveffi schema-version    # prints 0.9.0
```

## CLI reference

| Command | Description |
|---------|-------------|
| `weaveffi generate <file> -o <dir>` | Generate bindings from annotated Rust (`.rs`) or an IDL (`.yml`/`.json`/`.toml`); `--target c,swift,...` to subset, `--config weaveffi.toml` to override the auto-discovered project config, `--dry-run` to preview, `--force` to bypass the output cache |
| `weaveffi package <file> -o <dir>` | Assemble publishable, per-platform packages that bundle a prebuilt native library (Kotlin bundles `jniLibs/<abi>/`, Wasm bundles the `.wasm` into an npm package); `--binaries <dir>` for prebuilt libs or `--build <crate>` to cross-compile a Rust producer |
| `weaveffi validate <file>` | Validate an API definition without generating; `--warn` to also report advisory lints, `--format json` for machine-readable output |
| `weaveffi diff <file>` | Show what would change if bindings were regenerated; `--check` for CI |
| `weaveffi extract <file.rs>` | Derive an IDL document from `#[weaveffi::module]`-annotated Rust source |
| `weaveffi schema --format json-schema` | Print the JSON Schema for the IDL |
| `weaveffi schema-version` | Print the current IDL schema version (`0.9.0`) |
| `weaveffi completions <shell>` | Print shell completion scripts (`bash`, `zsh`, `fish`, `powershell`, `elvish`) |

### Project configuration: `weaveffi.toml`

The API definition describes only the API. Everything about how it's
published lives in an optional `weaveffi.toml` next to it (the CLI looks for
the nearest one at or above the input file; `--config` points at a specific
one):

```toml
[package]
name = "kvstore"          # stamped into every generated manifest
version = "0.1.0"
description = "An embedded key-value store"
license = "MIT"

[global]
c_prefix = "kv"           # rename the C ABI symbol prefix for every target

[generators.swift]
module_name = "KVStore"

[generators.node]
strip_module_prefix = true
```

Without a `weaveffi.toml`, a Rust producer's package name defaults to its
crate directory name, and every generator runs with its defaults. See
[Configuration](docs/src/guides/config.md) for every key.

Reference the JSON Schema from your IDL for editor autocompletion:

```yaml
# yaml-language-server: $schema=./weaveffi.schema.json
version: "0.9.0"
modules: ...
```

Regenerate the schema with `weaveffi schema --format json-schema > weaveffi.schema.json`.

## Documentation

Full documentation lives at <https://weaveffi.com/> (sources under
[`docs/`](./docs/)). Key pages:

- [Introduction](docs/src/intro.md): what WeaveFFI is and why it exists
- [Getting Started](docs/src/getting-started.md): install, define an IDL, generate, and call from C
- [C ABI Contract](docs/src/reference/abi.md): the normative description of ABI revision 2 (objects, callback interfaces, value buffers, async, iterators)
- [Comparison](docs/src/comparison.md): feature matrix vs UniFFI, Diplomat, cbindgen, swift-bridge, napi-rs, wasm-bindgen, SWIG, autocxx
- [FAQ](docs/src/faq.md): top questions about scope, object ownership, callbacks, runtime cost, and platform support
- [Samples](docs/src/samples.md): the eight sample producers, from `calculator` to the `codec` round-trip oracle
- [Generators](docs/src/generators/): per-target reference for each of the eleven languages
- [Guides](docs/src/guides/): the producer macro, memory ownership, error handling, async, configuration, packaging
- [Stability and Versioning](docs/src/stability.md): what changed in schema 0.9.0 / ABI 2 and how to migrate

## Status

WeaveFFI is in active `0.x` development. The current IDL schema is `0.9.0`
and the C ABI is at revision 2 (reference-counted objects, callback
interfaces, and a pluggable async spawner; `handle`, borrowed `&str`/`&[u8]`
spellings, module-level callbacks and listeners, and the arena runtime were
removed). Following [Semantic Versioning](https://semver.org/), the public
surface (the CLI, the IDL schema, the generated code, and the `weaveffi-abi`
runtime symbols) may change between minor releases while the project is
pre-1.0, and only the current IDL schema version is accepted. See [Stability
and Versioning](docs/src/stability.md) for the migration guide and the
recommended `weaveffi diff --check` CI workflow, and [Roadmap](docs/src/roadmap.md)
for what's planned next.

The full quality gate (`cargo fmt`, `cargo clippy -D warnings`, `cargo
test`, `cargo doc -D warnings`, `cargo deny`, `cargo audit`, `cargo
machete`, `cargo insta test --check`, `cargo bench --no-run`, and
`weaveffi diff --check` on every sample) runs in CI on every PR. The test
suite includes a property-based test of the value-buffer codec, `trybuild`
UI tests pinning the producer macro's compile-time diagnostics, and a
snapshot corpus of five IDL fixtures across all eleven generators. A
conformance harness builds real producers and runs generated consumers end
to end in all eleven languages, with dedicated async lanes and a `codec`
lane that round-trips every value-buffer wire shape (including object
tokens) through every language.

Releases are fully automated by [semantic-release](https://semantic-release.gitbook.io/)
on merge to `main`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow, snapshot
test conventions, fuzzing setup, and Conventional Commit rules.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
