# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.12.0-orange)](./weaveffi.schema.json) [![C ABI](https://img.shields.io/badge/C%20ABI-5-orange)](docs/src/reference/abi.md)

WeaveFFI generates idiomatic, type-safe bindings for 11 languages (C, C++,
Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart, Go, and Ruby) for any
native library that exposes a C ABI. Write the library in Rust and annotate a
module with `#[weaveffi::module]`, or describe it in a YAML or JSON IDL and
implement the generated C header in C, C++, Zig, or anything else. Either
way, every language talks to the same stable C ABI, and every package is named
after your library, not after WeaveFFI.

What you get, in every language:

- **Real objects.** Interfaces become classes backed by a reference-counted
  native object, released deterministically (`close()`, `Dispose()`,
  `deinit`, RAII) with a garbage-collector backstop, and safe to close while
  another thread is mid-call.
- **Typed errors.** An error domain becomes an exception hierarchy, a Swift
  error enum, or a Go error type the consumer can match on, and a domain
  can gain codes without breaking deployed bindings.
- **Async with cancellation.** `async` functions become `Promise`s, coroutines,
  `Task`s, `async throws`, or awaitables, and cancelling them (an
  `AbortSignal`, a `CancellationToken`, a `context.Context`, task
  cancellation) cancels the native work.
- **Callbacks.** A callback interface becomes a protocol, interface, or
  abstract class the consumer implements and the library calls from any
  thread.
- **Load-time safety.** Each generated package checks the library's ABI
  revision and its contract table before the first call, so a stale binding
  fails with an error naming the declaration that changed instead of
  corrupting memory.

## Quickstart

**1. Install the CLI.**

```bash
cargo install weaveffi-cli
```

**2. Write the library.**

```bash
cargo new --lib kvstore && cd kvstore
cargo add weaveffi
```

Put this in `src/lib.rs` (no `crate-type` is needed: WeaveFFI builds the
`cdylib` itself):

```rust
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// The store's errors. The macro generates `Display` from the docs.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum KvError {
        /// key not found
        KeyNotFound = 1001,
        /// store has reached capacity
        StoreFull = 1003,
    }

    /// An in-memory key-value store.
    #[weaveffi::interface]
    pub struct Store {
        entries: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl Store {
        /// Open a store.
        pub fn open(path: String) -> Result<Store, KvError> {
            let _ = path;
            Ok(Store { entries: Mutex::new(BTreeMap::new()) })
        }

        /// Insert `value` under `key`; true if the key is new.
        pub fn put(&self, key: &str, value: Vec<u8>) -> Result<bool, KvError> {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= 1024 && !entries.contains_key(key) {
                return Err(KvError::StoreFull);
            }
            Ok(entries.insert(key.to_string(), value).is_none())
        }

        /// The value under `key`.
        pub fn get(&self, key: &str) -> Result<Vec<u8>, KvError> {
            self.entries.lock().unwrap().get(key).cloned().ok_or(KvError::KeyNotFound)
        }

        /// The number of entries.
        pub fn count(&self) -> usize {
            self.entries.lock().unwrap().len()
        }
    }
}

// The C ABI runtime surface, once per cdylib.
weaveffi::export_runtime!();
```

The macro emits the `extern "C"` functions; you write no `unsafe` code. An
interface type must be `Send + Sync`, because the object is shared across the
boundary as an `Arc<T>`. Error messages come from the variants' doc comments
(or `#[weaveffi(message = "...")]` templates that interpolate fields), and a
`Result` whose error type isn't a declared domain (`String`,
`std::io::Error`, `anyhow::Error`) throws an untyped error with its
`Display` text. This is a trimmed-down store; the full
[`samples/kvstore`](samples/kvstore/src/lib.rs) adds records, enums,
callbacks, iterators, async, and the rest (see [Samples](docs/src/samples.md)).

**3. Generate bindings.** The package name, the C prefix (`kvstore_`), and
the library name all come from the crate.

```bash
weaveffi init           # writes weaveffi.toml: input = "." (this crate), targets = ["c", "python"]
weaveffi generate       # builds libkvstore, reads its API, writes the targets to ./bindings
```

`weaveffi generate` builds the crate and reads the API from the library it
compiled (the macro embeds it), so the bindings always describe exactly what
the library exports. While iterating, use `weaveffi dev` instead: it does the
same and also copies the fresh debug library into the generated Python,
Node.js, and Ruby packages (and prints how to point the other targets at it).

**4. Use them.** From Python, for example:

```bash
weaveffi dev                       # regenerate and copy libkvstore into bindings/python/kvstore
pip install ./bindings/python
```

```python
import kvstore

with kvstore.Store.open("data.kv") as store:
    store.put("greeting", b"hello")
    print(store.count())  # 1
    try:
        store.get("missing")
    except kvstore.KvError as e:
        print(e.code, e.message)  # 1001 key not found
```

<details>
<summary>The C header every target binds to (<code>bindings/c/kvstore.h</code>)</summary>

```c
#define KVSTORE_ABI_VERSION 5u
KVSTORE_API uint32_t kvstore_abi_version(void);

typedef struct kvstore_error {
    int32_t code;                /* 0 ok, >0 a KvError code, <0 a runtime code */
    const uint8_t* message_ptr;  /* UTF-8, not NUL-terminated */
    size_t message_len;
    const uint8_t* payload_ptr;  /* the code's fields, or NULL */
    size_t payload_len;
} kvstore_error;

KVSTORE_API const kvstore_contract_entry* kvstore_kv_contract(size_t* out_len);
#define KVSTORE_KV_CONTRACT_LEN 8

typedef int32_t kvstore_kv_KvError;
enum {
    kvstore_kv_KvError_KeyNotFound = 1001,
    kvstore_kv_KvError_StoreFull = 1003
};
typedef struct kvstore_kv_Store kvstore_kv_Store;

KVSTORE_API kvstore_kv_Store* kvstore_kv_Store_open(const uint8_t* path_ptr, size_t path_len, kvstore_error* out_err);
KVSTORE_API bool kvstore_kv_Store_put(const kvstore_kv_Store* self, const uint8_t* key_ptr, size_t key_len, const uint8_t* value_ptr, size_t value_len, kvstore_error* out_err);
KVSTORE_API const uint8_t* kvstore_kv_Store_get(const kvstore_kv_Store* self, const uint8_t* key_ptr, size_t key_len, size_t* out_len, kvstore_error* out_err);
KVSTORE_API uint64_t kvstore_kv_Store_count(const kvstore_kv_Store* self, kvstore_error* out_err);
KVSTORE_API kvstore_kv_Store* kvstore_kv_Store_clone(const kvstore_kv_Store* self);
KVSTORE_API void kvstore_kv_Store_destroy(kvstore_kv_Store* self);
```

Every symbol and type carries the library's own prefix (`kvstore_`), so any
number of WeaveFFI-built libraries can link into one process. Enums and
error codes are `int32_t` typedefs, never `typedef enum`, so their size is
fixed on every compiler, and a `usize` count crosses as `uint64_t`.

</details>

<details>
<summary>Swift (<code>bindings/swift/Sources/Kvstore/Kvstore.swift</code>)</summary>

```swift
public enum KvError: Error, LocalizedError, Hashable, Sendable {
    case keyNotFound(message: String)
    case storeFull(message: String)
    case unknown(code: Int32, message: String)  // a code from a newer library
}

public final class Store: Hashable, @unchecked Sendable {
    deinit { kvstore_kv_Store_destroy(ptr) }

    public static func open(path: String) throws -> Store { /* ... */ }
    public func put(key: String, value: Data) throws -> Bool { /* ... */ }
    public func get(key: String) throws -> Data { /* ... */ }
    public func count() -> UInt64 { /* ... */ }
}
```

</details>

<details>
<summary>Python, fully annotated (<code>bindings/python/kvstore/kvstore.py</code>)</summary>

```python
class KvError(Error):
    """Base exception of the `KvError` error domain (module `kv`)."""

class KeyNotFoundError(KvError):
    CODE = 1001

class StoreFullError(KvError):
    CODE = 1003

class Store(_Object):  # close() it, or use `with`
    @classmethod
    def open(cls, path: str) -> Store: ...
    def put(self, key: str, value: bytes) -> bool: ...
    def get(self, key: str) -> bytes: ...
    def count(self) -> int: ...
```

</details>

Not writing the library in Rust? Describe the same API in an IDL, run
`weaveffi init` outside a Cargo crate for a starter `api.yml`, generate
`--target c`, and implement the header. See [Getting
Started](docs/src/getting-started.md).

## Targets

| Target | Tier | Package | Objects | Async | Cancellation | Callbacks |
|---|---|---|---|---|---|---|
| C | 1 | `{library}.h` (+ `{library}_buffer.h` codecs) | `_clone` / `_destroy` | completion callback | cancel token | vtable struct |
| C++ | 1 | `{library}.hpp`, CMake target | RAII class | `std::future` | `CancelToken` | abstract class |
| Swift | 1 | SwiftPM package | `deinit` | `async throws` | task cancellation | protocol |
| Kotlin | 1 | Gradle (Android or JVM) | `close()` + cleaner | `suspend` | coroutine cancellation | interface |
| Node.js | 1 | npm package (N-API addon) | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |
| Python | 1 | `pyproject.toml` package (typed, `py.typed`) | `close()`, `with` | awaitable | task cancellation | abstract base class |
| .NET | 1 | `.csproj` (NuGet) | `Dispose()` over `SafeHandle` | `Task` | `CancellationToken` | interface |
| Go | 2 | Go module (cgo) | `Close()` | blocking call | `context.Context` | interface |
| Ruby | 2 | gem | `close` | blocking call | `cancel:` token | duck-typed object |
| Dart | 2 | pub package | `dispose()` + `NativeFinalizer` | `Future` | `CancelToken` | abstract class |
| WebAssembly | 2 | npm package (ESM) | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |

Tier 1 targets get new ABI work first; Tier 2 targets may lag a release
behind (see [Stability](docs/src/stability.md#target-tiers)).

The [capability matrix](docs/src/generators/README.md) and the per-language
pages have the details: type mapping, naming, loading, and known limits.

## Install

```bash
cargo install weaveffi-cli      # from source
cargo binstall weaveffi-cli     # prebuilt
```

Prebuilt archives for Linux (x64, arm64), macOS (x64, arm64), and Windows
(x64) are attached to every [GitHub
release](https://github.com/weavefoundry/weaveffi/releases).

## CLI

| Command | What it does |
|---|---|
| `weaveffi init [dir]` | Write `weaveffi.toml` (and a starter IDL outside a Rust crate) |
| `weaveffi generate [input]` | Generate bindings from a Rust producer crate (built first; `--library <path>` reads an existing build) or an IDL; `--target c,swift` to subset, `--dry-run` to list files, `--check` to exit 1 when anything would change (for CI), `--diff` to print the changes. Only changed files are rewritten, and files a previous run produced that are no longer generated are removed |
| `weaveffi validate [input]` | Validate without generating; `--warn` for lints, `--format json` |
| `weaveffi build [input]` | Cross-compile the Rust producer per platform (`--platforms`) into `target/weaveffi/<platform>/`, with prebuilt Node.js and JNI glue |
| `weaveffi package [input]` | Build, then write installable artifacts per target to `dist/`: wheels, npm tarballs, gems, an `XCFramework` SwiftPM package, a `.nupkg`, and more |
| `weaveffi dev [input]` | Build the producer's library, generate, and point the bindings at the library |
| `weaveffi extract [input]` | Print the IDL a Rust producer's built library embeds (`--library <path>` to read a given build) |
| `weaveffi schema` | Print the JSON Schema of the IDL (`--version` prints its version) |
| `weaveffi completions <shell>` | Print shell completions |

Commands that build a Rust producer take `--profile <name>`, the Cargo
profile to build with: `dev` for `generate`, `validate`, `extract`, and
`dev`, and `release` (or `[build] profile`) for `build` and `package`.
`build` and `package` skip, with a warning, an artifact whose external tool
(a C compiler, Xcode, the .NET SDK) is missing, and fail on it only with
`--strict`.

With a `[project]` table in `weaveffi.toml`, every command works without
arguments from anywhere in the project:

```toml
[project]
input = "."              # this crate, or an IDL such as "kvstore.yml"
out = "bindings"
targets = ["c", "swift", "kotlin", "python"]

[package]
version = "1.0.0"
license = "MIT"

[generators.swift]
name = "KVStore"
```

A Rust producer's package name, C prefix, and library name come from its
`Cargo.toml`; an IDL's come from `[package]`. See
[Configuration](docs/src/guides/config.md).

## Documentation

The book lives at <https://weaveffi.com/> (sources in [`docs/`](docs/)):
[Getting Started](docs/src/getting-started.md), [the producer
macro](docs/src/guides/producer-macro.md), [errors and
memory](docs/src/guides/errors-and-memory.md), [async and
cancellation](docs/src/guides/async.md), the [IDL
reference](docs/src/reference/idl.md), the normative [C ABI
contract](docs/src/reference/abi.md), and a
[comparison](docs/src/comparison.md) with UniFFI, Diplomat, cbindgen, and
others.

## Status

WeaveFFI is pre-1.0: the IDL schema is `0.12.0` and the C ABI is revision 5,
and either may still change in a minor release. Revision 5 is designed to be
extended additively (new declarations, new callback methods, and new error
codes don't need a revision 6), and every binding checks the revision and a
per-declaration contract table when it loads the library. See [Stability and
Versioning](docs/src/stability.md) for the policy and the migration notes, and
the [Roadmap](docs/src/roadmap.md) for what's next.

The targets come in two tiers that pass the same conformance gate:

- **Tier 1**: C, C++, Swift, Kotlin, Python, Node.js, and .NET. These get
  new ABI and IDL features first, and their output is held to the strictest
  review.
- **Tier 2**: Go, Ruby, Dart, and WebAssembly. These are fully supported and
  conformance-tested, but may lag Tier 1 when the ABI grows.

Every pull request runs formatting, clippy, rustdoc, the test suite and
snapshot corpus on Linux, macOS, and Windows, a compile check of every
snapshot fixture with each language's own toolchain, and an end-to-end
conformance harness that builds real producers and runs generated consumers in
all 11 languages on Linux and macOS, asserting that every native object,
callback, iterator, token, and allocation is released by the end of each run.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
