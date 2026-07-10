# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.5.0-orange)](./weaveffi.schema.json) [![downloads](https://img.shields.io/crates/d/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli)

WeaveFFI generates type-safe bindings for 11 languages for any native library
that exposes a C ABI, whether it's written in Rust, C, C++, Zig, or anything
else: no hand-written JNI, no duplicate implementations, no unsafe boilerplate.
Define your API once as an IDL in YAML, JSON, or TOML and ship idiomatic
packages for C, C++, Swift, Kotlin/Android, Node.js, WebAssembly, Python, .NET,
Dart, Go, and Ruby that all talk to the same stable C ABI. Interfaces become
real classes with methods and automatic cleanup, and error domains become
typed errors consumers can catch and match on, not flat functions and raw
integer codes. Writing your producer in Rust? Annotate a normal module with
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
declares. An interface is a real object with methods; an error domain plus
`throws: true` gives its fallible members typed errors:

```yaml
version: "0.5.0"
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
and derives the IDL for you, so you write no `unsafe` glue. See
[The Rust Producer Macro](docs/src/guides/producer-macro.md) for the full
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
Every target gets a real `Store` class whose objects clean up after
themselves, and a typed `KvError` consumers can catch and match on. Click
each block below to see what WeaveFFI emits.

<details>
<summary><strong>C</strong>: <code>generated/c/weaveffi.h</code></summary>

```c
typedef struct weaveffi_kv_Store weaveffi_kv_Store;

typedef enum {
    weaveffi_kv_KvError_KeyNotFound = 1001,
    weaveffi_kv_KvError_StoreFull = 1003
} weaveffi_kv_KvError;

weaveffi_kv_Store* weaveffi_kv_Store_open(
    const char* path, weaveffi_error* out_err);

bool weaveffi_kv_Store_put(
    const weaveffi_kv_Store* self,
    const char* key,
    const uint8_t* value_ptr, size_t value_len,
    weaveffi_error* out_err);

int64_t weaveffi_kv_Store_count(
    const weaveffi_kv_Store* self, weaveffi_error* out_err);

void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
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
    // The native object is released automatically on deinit.

    public static func open(path: String) throws -> Store { /* ... */ }

    public func put(key: String, value: Data) throws -> Bool { /* ... */ }

    public func count() -> Int64 { /* ... */ }
}
```

</details>

<details>
<summary><strong>Python</strong>: <code>generated/python/weaveffi/weaveffi.pyi</code></summary>

```python
class KvError(WeaveFFIError): ...

class KeyNotFound(KvError):
    CODE: int  # 1001

class StoreFull(KvError):
    CODE: int  # 1003

class Store:
    @classmethod
    def open(cls, path: str) -> "Store": ...
    def put(self, key: str, value: bytes) -> bool: ...
    def count(self) -> int: ...
```

</details>

The remaining targets follow the same pattern with their own idioms: an
owned class (or the closest analogue) wired to the object's destructor, and
the module's error domain as a typed error or exception.

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
  Interfaces become real classes with methods and automatic disposal, and
  error domains become typed errors (a Swift error enum, Python exception
  classes, and each remaining target's own exception idiom). Swift gets
  `async/await` and `throws`, Kotlin gets `suspend` and JNI glue, Python
  gets typed `.pyi` stubs, TypeScript gets `Promise`s, and Dart gets
  `dart:ffi`, all from the same definition.
- **The whole IDL surface, on every target.** Interfaces, typed error
  domains, async functions, iterators, callbacks, and event listeners work
  across all eleven languages (Wasm excepts callbacks/listeners and says so
  loudly). Generators declare their capabilities and `weaveffi generate`
  fails with a clear error (never a silent skip) if a target can't deliver
  a feature you use. See the
  [feature matrix](docs/src/generators/README.md#feature-support-matrix).

## How does it compare?

See [Comparison](docs/src/comparison.md) for a side-by-side feature matrix
versus UniFFI, cbindgen, diplomat, SWIG, and autocxx, plus an honest
"when to choose WeaveFFI" guide.

## Supported targets

| Target | Output directory | What you get |
|--------|------------------|--------------|
| **C** | `c/` | `weaveffi.h` header with struct typedefs, function prototypes, and the shared `weaveffi_error` type |
| **C++** | `cpp/` | RAII header (`weaveffi.hpp`) with move semantics, `std::optional`/`std::vector`/`std::unordered_map` wrappers, exception-based errors, and a `CMakeLists.txt` |
| **Swift** | `swift/` | SwiftPM package wrapping the C ABI with `throws`, `async/await`, and `Codable`-friendly types |
| **Android** | `android/` | Kotlin JNI wrapper, C shim, and a Gradle project skeleton |
| **Node.js** | `node/` | N-API addon loader + TypeScript declarations and a `package.json` |
| **Wasm** | `wasm/` | JavaScript loader + TypeScript declarations for `wasm32-unknown-unknown` builds |
| **Python** | `python/` | `ctypes` bindings + `.pyi` type stubs + `pyproject.toml` |
| **.NET** | `dotnet/` | C# P/Invoke bindings + `.csproj` + `.nuspec` for NuGet |
| **Dart** | `dart/` | `dart:ffi` bindings + `pubspec.yaml` for Flutter and Dart projects |
| **Go** | `go/` | CGo bindings + `go.mod` for Go modules |
| **Ruby** | `ruby/` | FFI gem bindings + `.gemspec` for RubyGems |

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
weaveffi schema-version    # prints 0.5.0
```

## CLI reference

| Command | Description |
|---------|-------------|
| `weaveffi new <name>` | Scaffold a new project with a starter IDL and `Cargo.toml` |
| `weaveffi generate <file> -o <dir>` | Generate bindings from annotated Rust (`.rs`) or an IDL (`.yml`/`.json`/`.toml`); `--target c,swift,...` to subset, `--config cfg.toml` for options, `--scaffold` to emit Rust FFI stubs (for non-macro producers), `--dry-run` to preview |
| `weaveffi package <file> -o <dir>` | Assemble publishable, per-platform packages that bundle a prebuilt native library; `--binaries <dir>` for prebuilt libs or `--build <crate>` to cross-compile a Rust producer |
| `weaveffi validate <file>` | Validate an IDL definition without generating; `--format json` for machine-readable output |
| `weaveffi lint <file>` | Lint an IDL and report non-fatal warnings |
| `weaveffi diff <file>` | Show what would change if bindings were regenerated; `--check` for CI |
| `weaveffi extract <file.rs>` | Derive an IDL from `#[weaveffi::module]`-annotated Rust source |
| `weaveffi format <file>` | Rewrite an IDL file in canonical form (sorted keys); `--check` for CI |
| `weaveffi watch <file>` | Re-run `generate` whenever the IDL file changes |
| `weaveffi schema --format json-schema` | Print the JSON Schema for the IDL |
| `weaveffi schema-version` | Print the current IR schema version (`0.5.0`) |
| `weaveffi doctor` | Check for required toolchains; `--target swift` to scope to one language, `--format json` for CI |
| `weaveffi completions <shell>` | Print shell completion scripts (`bash`, `zsh`, `fish`, `powershell`, `elvish`) |

Reference the JSON Schema from your IDL for editor autocompletion:

```yaml
# yaml-language-server: $schema=./weaveffi.schema.json
version: "0.5.0"
modules: ...
```

Regenerate the schema with `weaveffi schema --format json-schema > weaveffi.schema.json`.

## Documentation

Full documentation lives at <https://weaveffi.com/> (sources under
[`docs/`](./docs/)). Key pages:

- [Introduction](docs/src/intro.md): what WeaveFFI is and why it exists
- [Getting Started](docs/src/getting-started.md): install → IDL → generate → call from C
- [Comparison](docs/src/comparison.md): feature matrix vs UniFFI, cbindgen, diplomat, SWIG, autocxx
- [FAQ](docs/src/faq.md): top questions about scope, runtime cost, and platform support
- [Generators](docs/src/generators/): per-target reference for each of the eleven languages
- [Guides](docs/src/guides/): memory ownership, error handling, async, configuration

## Status

WeaveFFI is in active `0.x` development. Following [Semantic
Versioning](https://semver.org/), the public surface (the CLI, the IDL
schema, the generated code, and the `weaveffi-abi` runtime symbols) may
change between minor releases while the project is pre-1.0, and only the
current IDL schema version is accepted. See [Stability and
Versioning](docs/src/stability.md) for what that means in practice and the
recommended `weaveffi diff --check` CI workflow.

The full quality gate (`cargo fmt`, `cargo clippy -D warnings`, `cargo
test`, `cargo doc -D warnings`, `cargo deny`, `cargo audit`, `cargo
machete`, `cargo insta test --check`, `cargo bench --no-run`, and
`weaveffi diff --check` on every sample) runs in CI on every PR.

Releases are fully automated by [semantic-release](https://semantic-release.gitbook.io/)
on merge to `main`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow, snapshot
test conventions, fuzzing setup, and Conventional Commit rules.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
