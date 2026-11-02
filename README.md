# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.13.0-orange)](./weaveffi.schema.json) [![C ABI](https://img.shields.io/badge/C%20ABI-6-orange)](docs/src/reference/abi.md)

WeaveFFI generates idiomatic, type-safe bindings for 11 languages (C, C++,
Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart, Go, and Ruby) for any
native library that exposes a C ABI. Write the library in Rust and tag the
items you export with `#[weaveffi::export]` and its siblings, or describe it
in a YAML or JSON IDL and implement the generated C header in C, C++, Zig, or
anything else. Either way, every language talks to the same stable C ABI,
and every package is named after your library, not after WeaveFFI.

What you get, in every language:

- **Real objects.** Interfaces become classes backed by a reference-counted
  native object, released deterministically (`close()`, `Dispose()`,
  `deinit`, RAII) with a garbage-collector backstop, and safe to close while
  another thread is mid-call.
- **Typed errors.** An error domain becomes an exception hierarchy, a Swift
  error enum, or a Go error type the consumer can match on, and a domain
  can gain codes without breaking deployed bindings.
- **Async with cancellation.** `async` functions become `Promise`s,
  coroutines, `Task`s, `async throws`, or awaitables, and cancelling them (an
  `AbortSignal`, a `CancellationToken`, a `context.Context`, task
  cancellation) cancels the native work.
- **Callbacks.** A callback interface becomes a protocol, interface, or
  abstract class the consumer implements and the library calls from any
  thread.
- **Load-time safety.** Each generated package checks the library's ABI
  revision and the contract of every declaration before the first call, so
  a stale binding fails with an error naming the declaration that changed
  instead of corrupting memory.

## Quickstart

**1. Install the CLI.**

```bash
cargo install weaveffi-cli
```

**2. Write the library.**

```bash
cargo new --lib mathlib && cd mathlib
cargo add weaveffi
```

Put this in `src/lib.rs` (no `crate-type` is needed: WeaveFFI builds the
`cdylib` itself):

```rust
/// Errors the math functions report.
#[weaveffi::error]
#[derive(Debug)]
pub enum MathError {
    /// division by zero
    DivisionByZero = 1,
}

/// Add two integers, wrapping on overflow.
#[weaveffi::export]
pub fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

/// Divide `a` by `b`, failing when `b` is zero.
#[weaveffi::export]
pub fn div(a: i32, b: i32) -> Result<i32, MathError> {
    a.checked_div(b).ok_or(MathError::DivisionByZero)
}

/// Greet someone by name.
#[weaveffi::export]
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

// The runtime symbols (errors, memory, cancel tokens), once per library.
weaveffi::export_runtime!();
```

The attributes emit the `extern "C"` functions; you write no `unsafe` code.
Error messages come from the variants' doc comments (or
`#[weaveffi(message = "...")]` templates that interpolate fields), and a
`Result` whose error type is untyped (`String`, `std::io::Error`,
`weaveffi::Error`) throws a message without a code. This is the
[`examples/getting-started`](examples/getting-started/src/lib.rs) crate,
which the workspace tests; [`examples/macro-tour`](examples/macro-tour/src/lib.rs)
adds records, enums, objects, callbacks, iterators, async, and custom types,
and the samples in [`samples/`](docs/src/samples.md) use every feature.

**3. Generate bindings.** The package name, the C prefix (`mathlib_`), and
the library name all come from the crate.

```bash
weaveffi init           # writes weaveffi.toml: input = "." (this crate), targets = ["c", "python"]
weaveffi generate       # builds libmathlib, reads its API, writes the targets to ./bindings
```

`weaveffi generate` builds the crate (with your Cargo features, `-F`) and
reads the API from the library it compiled (the attributes embed it), so the
bindings always describe exactly what the library exports. While iterating,
add `--dev`: it also copies the fresh debug library into the generated
packages that bundle one, and prints how to point the other targets at it.

**4. Use them.** From Python, for example:

```bash
weaveffi generate --dev
pip install ./bindings/python
```

```python
import mathlib

print(mathlib.add(2, 3))   # 5
try:
    mathlib.div(1, 0)
except mathlib.MathError as e:
    print(e)               # (1) division by zero
```

<details>
<summary>The C header every target binds to (<code>bindings/c/mathlib.h</code>)</summary>

```c
#define MATHLIB_ABI_VERSION 6u
MATHLIB_API uint32_t mathlib_abi_version(void);

/* Every string, bytes, typed array, and value buffer crosses as one run. */
typedef struct mathlib_run { const uint8_t* ptr; size_t len; } mathlib_run;

typedef struct mathlib_error {
    int32_t code;                /* 0 ok, >0 a MathError code, <0 a runtime code */
    const uint8_t* message_ptr;  /* UTF-8, not NUL-terminated */
    size_t message_len;
    const uint8_t* payload_ptr;  /* the code's fields, or NULL */
    size_t payload_len;
} mathlib_error;

/* One contract function per declaration; this checks them all. */
static inline const char* mathlib_contract_check(void);

typedef int32_t mathlib_MathError;
enum {
    mathlib_MathError_DivisionByZero = 1
};

MATHLIB_API int32_t mathlib_add(int32_t a, int32_t b, mathlib_error* out_err);
MATHLIB_API int32_t mathlib_div(int32_t a, int32_t b, mathlib_error* out_err);
MATHLIB_API mathlib_run mathlib_greet(mathlib_run name, mathlib_error* out_err);
MATHLIB_API void mathlib_run_free(mathlib_run run);
```

Every symbol and type carries the library's own prefix (`mathlib_`), so any
number of WeaveFFI-built libraries can link into one process. Every value is
one C value; enums and error codes are `int32_t` typedefs, never `typedef
enum`, so their size is fixed on every compiler.

</details>

Not writing the library in Rust? Describe the same API in an IDL (one file,
or several with `imports:`), run `weaveffi init` outside a Cargo crate for a
starter, generate `--target c`, and implement the header. See [Getting
Started](docs/src/getting-started.md).

## Targets

| Target | Tier | Package | Objects | Async | Cancellation | Callbacks |
|---|---|---|---|---|---|---|
| C | 1 | `{library}.h` (+ `{library}_buffer.h` codecs) | `_clone` / `_destroy` | completion callback | cancel token | `{ctx, vtable}` struct |
| C++ | 1 | header over the C header, CMake target | RAII class | `std::future` | cancel token | abstract class |
| Swift | 1 | SwiftPM package | `deinit` | `async throws` | task cancellation | protocol |
| Kotlin | 1 | Gradle (Android or JVM), through JNA | `close()` + cleaner | `suspend` | coroutine cancellation | interface |
| Node.js | 1 | npm package, through koffi | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |
| Python | 1 | `pyproject.toml` package | `close()`, `with` | awaitable | task cancellation | abstract base class |
| .NET | 1 | `.csproj` (NuGet) | `Dispose()` | `Task` | `CancellationToken` | interface |
| Go | 2 | Go module (cgo) | `Close()` | blocking call | `context.Context` | interface |
| Ruby | 2 | gem | `close` | blocking call | cancel token | duck-typed object |
| Dart | 2 | pub package | `dispose()` + finalizer | `Future` | cancel token | abstract class |
| WebAssembly | 2 | npm package (ESM) | `close()`, `Symbol.dispose` | `Promise` | `AbortSignal` | object |

No target needs compiled glue: each binds the C ABI through its language's
own FFI. Tier 1 targets get new ABI work first; Tier 2 targets may lag a
release behind (see [Stability](docs/src/stability.md#target-tiers)). The
[capability matrix](docs/src/generators/README.md) and the per-language
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
| `weaveffi generate [input]` | Generate bindings from a Rust producer crate (built first; `--library <path>` reads an existing build) or an IDL; `--target c,swift` to subset, `--dry-run` to list files, `--check` to exit 1 when anything would change (for CI), `--diff` to print the changes, `--dev` to point the bindings at the producer's library. Only changed files are rewritten, and files a previous run produced that are no longer generated are removed |
| `weaveffi validate [input]` | Validate without generating; `--warn` for lints, `--format json` |
| `weaveffi build [input]` | Cross-compile the Rust producer per platform (`--platforms`) into `target/weaveffi/<platform>/` |
| `weaveffi package [input]` | Build, then write installable artifacts per target to `dist/`: wheels, npm tarballs, gems, an `XCFramework` SwiftPM package, a `.nupkg`, and more |
| `weaveffi extract [input]` | Print the IDL a Rust producer's built library embeds (`-o` to write a file, `-f json` for JSON, `--library <path>` to read a given build) |
| `weaveffi schema` | Print the JSON Schema of the IDL (`--version` prints its version) |
| `weaveffi completions <shell>` | Print shell completions |

Commands that build a Rust producer take `--profile <name>` (`dev` for
`generate`, `validate`, and `extract`; `release`, or `[build] profile`, for
`build` and `package`) and the Cargo feature flags `-F`/`--features`,
`--all-features`, and `--no-default-features`, which reach every build,
including the one the API is read from. `build` and `package` skip, with a
warning, an artifact whose external tool (a C compiler, Xcode, the .NET SDK)
is missing, and fail on it only with `--strict`.

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

[build]
features = ["sync"]      # Cargo features every build uses

[generators.swift]
name = "KVStore"
```

A Rust producer's package name, C prefix, and library name come from its
`Cargo.toml`; an IDL's come from `[package]`. Commit the generated bindings
with their generation records (`bindings/.weaveffi/`) and gate CI on
`weaveffi generate --check`. See
[Configuration](docs/src/guides/config.md).

## Documentation

The book lives at <https://weaveffi.com/> (sources in [`docs/`](docs/)),
with each release's book under `/v{major}.{minor}/` and the development
head under `/dev/`: [Getting Started](docs/src/getting-started.md), [the
producer macros](docs/src/guides/producer-macro.md), [errors and
memory](docs/src/guides/errors-and-memory.md), [async and
cancellation](docs/src/guides/async.md), the [IDL
reference](docs/src/reference/idl.md), the normative [C ABI
contract](docs/src/reference/abi.md), and a
[comparison](docs/src/comparison.md) with UniFFI, Diplomat, cbindgen, and
others. Every Rust snippet in the guides is included from a crate in
[`examples/`](examples/) that the workspace compiles and tests.

## Status

WeaveFFI is pre-1.0: the IDL schema is `0.13.0` and the C ABI is revision 6,
and either may still change in a minor release. Revision 6 is designed to be
extended additively (new declarations, new callback methods, and new error
codes don't need a new revision), and every binding checks the revision and
each declaration's contract when it loads the library. See [Stability and
Versioning](docs/src/stability.md) for the policy and the
[Roadmap](docs/src/roadmap.md) for what's next.

The targets come in two tiers that pass the same conformance gate:

- **Tier 1**: C, C++, Swift, Kotlin, Python, Node.js, and .NET. These get
  new ABI and IDL features first, and their output is held to the strictest
  review.
- **Tier 2**: Go, Ruby, Dart, and WebAssembly. These are fully supported and
  conformance-tested, but may lag Tier 1 when the ABI grows.

Every pull request runs formatting, clippy, rustdoc, the test suite and
snapshot corpus on Linux, macOS, and Windows, a compile check of every
snapshot fixture with each language's own toolchain, and an end-to-end
conformance harness that builds real producers and runs generated consumers
in all 11 languages on Linux and macOS, asserting that every native object,
callback, iterator, token, and allocation is released by the end of each run.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
