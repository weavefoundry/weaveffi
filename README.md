# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.11.0-orange)](./weaveffi.schema.json) [![C ABI](https://img.shields.io/badge/C%20ABI-4-orange)](docs/src/reference/abi.md)

WeaveFFI generates idiomatic, type-safe bindings for 11 languages (C, C++,
Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart, Go, and Ruby) for any
native library that exposes a C ABI. Write the library in Rust and annotate a
module with `#[weaveffi::module]`, or describe it in a YAML, JSON, or TOML IDL
and implement the generated C header in C, C++, Zig, or anything else. Either
way, every language talks to the same stable C ABI, and every package is named
after your library, not after WeaveFFI.

What you get, in every language:

- **Real objects.** Interfaces become classes backed by a reference-counted
  native object, released deterministically (`close()`, `Dispose()`,
  `deinit`, RAII) with a garbage-collector backstop, and safe to close while
  another thread is mid-call.
- **Typed errors.** An error domain becomes an exception hierarchy, a Swift
  error enum, or a Go error type the consumer can match on.
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

Add `crate-type = ["cdylib"]` under `[lib]` in `Cargo.toml`, then put this in
`src/lib.rs`:

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

    impl std::fmt::Display for KvError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                KvError::KeyNotFound => "key not found",
                KvError::StoreFull => "store has reached capacity",
            })
        }
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

        /// The number of entries.
        pub fn count(&self) -> i64 {
            self.entries.lock().unwrap().len() as i64
        }
    }
}

// The C ABI runtime surface, once per cdylib.
weaveffi::export_runtime!();
```

The macro emits the `extern "C"` functions; you write no `unsafe` code. An
interface type must be `Send + Sync`, because the object is shared across the
boundary as an `Arc<T>`. This is a trimmed-down store; the full
[`samples/kvstore`](samples/kvstore/src/lib.rs) adds records, enums,
callbacks, iterators, async, and the rest (see [Samples](docs/src/samples.md)).

**3. Generate bindings.** The package name, the C prefix (`kvstore_`), and
the library name all come from the crate.

```bash
weaveffi init           # writes weaveffi.toml: [project] input = "." (this crate)
weaveffi generate       # builds libkvstore, reads its API, writes every target to ./bindings
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
        store.put("another", b"value")
    except kvstore.StoreFull as e:
        print(e.code, e.message)  # 1003 store has reached capacity
```

<details>
<summary>The C header every target binds to (<code>bindings/c/kvstore.h</code>)</summary>

```c
#define KVSTORE_ABI_VERSION 4u
KVSTORE_API uint32_t kvstore_abi_version(void);
KVSTORE_API const kvstore_contract_entry* kvstore_kv_contract(size_t* out_len);
#define KVSTORE_KV_CONTRACT_LEN 5

typedef enum {
    kvstore_kv_KvError_KeyNotFound = 1001,
    kvstore_kv_KvError_StoreFull = 1003
} kvstore_kv_KvError;
typedef struct kvstore_kv_Store kvstore_kv_Store;

KVSTORE_API kvstore_kv_Store* kvstore_kv_Store_open(const uint8_t* path_ptr, size_t path_len, kvstore_error* out_err);
KVSTORE_API bool kvstore_kv_Store_put(const kvstore_kv_Store* self, const uint8_t* key_ptr, size_t key_len, const uint8_t* value_ptr, size_t value_len, kvstore_error* out_err);
KVSTORE_API int64_t kvstore_kv_Store_count(const kvstore_kv_Store* self, kvstore_error* out_err);
KVSTORE_API kvstore_kv_Store* kvstore_kv_Store_clone(const kvstore_kv_Store* self);
KVSTORE_API void kvstore_kv_Store_destroy(kvstore_kv_Store* self);
```

Every symbol and type carries the library's own prefix (`kvstore_`), so any
number of WeaveFFI-built libraries can link into one process.

</details>

<details>
<summary>Swift (<code>bindings/swift/Sources/Kvstore/Kvstore.swift</code>)</summary>

```swift
public enum KvError: Error, LocalizedError, Sendable {
    case keyNotFound(message: String)
    case storeFull(message: String)
}

public final class Store: @unchecked Sendable {
    deinit { kvstore_kv_Store_destroy(ptr) }

    public static func open(path: String) throws -> Store { /* ... */ }
    public func put(key: String, value: Data) throws -> Bool { /* ... */ }
    public func count() -> Int64 { /* ... */ }
}
```

</details>

<details>
<summary>Python, fully annotated (<code>bindings/python/kvstore/kvstore.py</code>)</summary>

```python
class KvError(Error):
    """Base exception for the `kv` module's error domain."""

class KeyNotFound(KvError):
    CODE = 1001

class StoreFull(KvError):
    CODE = 1003

class Store(_Object):  # close() it, or use `with`
    @classmethod
    def open(cls, path: str) -> Store:
        _path_b = path.encode("utf-8")
        _err = _ErrorStruct()
        _ret = _c_kv_Store_open(_path_b, len(_path_b), ctypes.byref(_err))
        if _err.code:
            raise _kv_error_from(*_read_error(_err))
        return cls._adopt(_required(_ret))

    def put(self, key: str, value: bytes) -> bool: ...
    def count(self) -> int: ...
```

</details>

Not writing the library in Rust? Describe the same API in an IDL, run
`weaveffi init` outside a Cargo crate for a starter `api.yml`, generate
`--target c`, and implement the header. See [Getting
Started](docs/src/getting-started.md).

## Targets

| Target | Package | Objects | Async | Cancellation | Callbacks |
|---|---|---|---|---|---|
| C | `{library}.h` (+ `{library}_buffer.h` codecs) | `_clone` / `_destroy` | completion callback | cancel token | vtable struct |
| C++ | `{library}.hpp`, CMake target | RAII class | `std::future` | `CancelToken` | abstract class |
| Swift | SwiftPM package | `deinit` | `async throws` | task cancellation | `protocol` |
| Kotlin | Gradle (Android or JVM) | `AutoCloseable` + cleaner | `suspend` | coroutine cancellation | `interface` |
| Node.js | npm package (N-API addon) | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |
| WebAssembly | npm package (ESM) | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |
| Python | `pyproject.toml` package (typed, `py.typed`) | `close()`, `with` | awaitable | task cancellation | ABC |
| .NET | `.csproj` (NuGet) | `IDisposable` over `SafeHandle` | `Task` | `CancellationToken` | interface |
| Dart | pub package | `dispose()` + `NativeFinalizer` | `Future` | `CancelToken` | abstract class |
| Go | Go module (cgo) | `Close() error` | blocking call | `context.Context` | interface |
| Ruby | gem | `close` | blocking call | `cancel:` token | module |

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
| `weaveffi generate [input]` | Generate bindings from a Rust producer crate (built first; `--library <path>` reads an existing build) or an IDL; `--target c,swift` to subset, `--dry-run` to list files. Only changed files are rewritten, and files a previous run produced that are no longer generated are removed |
| `weaveffi validate [input]` | Validate without generating; `--warn` for lints, `--format json` |
| `weaveffi diff [input]` | Show what regenerating would change; `--check` exits non-zero for CI |
| `weaveffi build [input]` | Cross-compile the Rust producer per platform (`--platforms`) into `target/weaveffi/<platform>/`, with prebuilt Node.js and JNI glue |
| `weaveffi package [input]` | Build, then write installable artifacts per target to `dist/`: wheels, npm tarballs, gems, an `XCFramework` SwiftPM package, a `.nupkg`, and more |
| `weaveffi dev [input]` | Build the producer's debug library, generate, and point the bindings at the library |
| `weaveffi extract [input]` | Print the IDL a Rust producer's built library embeds (`--library <path>` to read a given build) |
| `weaveffi schema` | Print the JSON Schema of the IDL |

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

WeaveFFI is pre-1.0: the IDL schema is `0.11.0` and the C ABI is revision 4,
and either may change in a minor release. See [Stability and
Versioning](docs/src/stability.md) for the policy and the migration notes, and
the [Roadmap](docs/src/roadmap.md) for what's next.

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
