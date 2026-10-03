# Introduction

**WeaveFFI generates idiomatic bindings for 11 languages from one API
definition, over a stable C ABI.** Write the API as safe Rust annotated with
`#[weaveffi::module]`, or as a YAML, JSON, or TOML IDL implemented by any
library that can export C symbols (C, C++, Zig, and so on). WeaveFFI emits
packages for C, C++, Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart,
Go, and Ruby that all call the same C functions.

## The design in one page

**One definition, two front doors.** A Rust producer annotates ordinary
modules; the `#[weaveffi::module]` macro emits the `extern "C"` thunks, and
the CLI reads the same source to emit the bindings. Both run one shared
extractor, so the library you compile and the bindings you ship are two views
of one parse. A non-Rust producer writes an [IDL](reference/idl.md) instead
and implements the C header WeaveFFI generates from it.

**Identity drives every name.** A library has one resolved
[identity](reference/naming.md): a package `name`, a C symbol `prefix`, and a
native `library` base name. For a Rust producer all three come from the crate
(`kvstore` gives `kvstore_kv_Store_open`, `libkvstore.so`, and an npm, PyPI,
or NuGet package named `kvstore`). Nothing a generator emits is named after
WeaveFFI, so any number of WeaveFFI-built libraries can live in one process.

**A small, explicit C ABI.** [Revision 3](reference/abi.md) has five value
families: direct scalars, UTF-8 strings and bytes as `(ptr, len)` runs,
serialized [value buffers](reference/value-buffers.md) for records, enums,
optionals, lists, and maps, reference-counted objects, and
consumer-implemented callback vtables. Every fallible call reports through a
`{prefix}_error` struct. Async functions complete through a callback that
fires exactly once, and cancellation is a first-class runtime code. Every
consumer checks the ABI revision and a per-module contract checksum when it
loads the library, so a stale binding fails loudly instead of misreading
memory.

**Idiomatic generators.** Each target maps the model onto its own idioms:
objects become classes with deterministic disposal and a garbage-collector
backstop, async functions become `async`/`await`, coroutines, promises, or
tasks with native cancellation, iterators stay lazy, and error domains
become typed exceptions or error values. The
[capability matrix](generators/README.md) summarizes all eleven.

**Standalone output.** Generated packages are self-contained: helper code
(the value-buffer codec, error types, object wrappers, loaders) is generated
into each package, so consumers never install WeaveFFI. `weaveffi package`
bundles prebuilt native libraries into publishable packages per ecosystem.

## Where to next

- [Getting Started](getting-started.md): a Rust producer called from Python
  and C in a few minutes, plus the IDL path for a C implementation.
- [The Rust Producer Macro](guides/producer-macro.md): every supported
  signature and what the macro rejects.
- [C ABI Contract](reference/abi.md): the normative description of the
  boundary.
- [Comparison](comparison.md) and [FAQ](faq.md): how WeaveFFI relates to
  UniFFI, Diplomat, cbindgen, and the single-language bridges.
- [Stability and Versioning](stability.md): what changed in schema 0.10 and
  ABI 3, and how to migrate.
