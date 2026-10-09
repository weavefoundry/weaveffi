# Introduction

**WeaveFFI generates idiomatic bindings for 11 languages from one API
definition, over a stable C ABI.** Write the API as safe Rust annotated with
`#[weaveffi::module]`, or as a YAML or JSON IDL implemented by any
library that can export C symbols (C, C++, Zig, and so on). WeaveFFI emits
packages for C, C++, Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart,
Go, and Ruby that all call the same C functions.

## The design in one page

**One definition, two front doors.** A Rust producer annotates ordinary
modules; the `#[weaveffi::module]` macro emits the `extern "C"` thunks and
embeds a description of the API in the compiled library, and the CLI reads
that description back out to emit the bindings, so the bindings you ship
describe exactly the library you compiled, `#[cfg]` included. A non-Rust
producer writes an [IDL](reference/idl.md) instead and implements the C
header WeaveFFI generates from it.

**Identity drives every name.** A library has one resolved
[identity](reference/naming.md): a package `name`, a C symbol `prefix`, and a
native `library` base name. For a Rust producer all three come from the crate
(`kvstore` gives `kvstore_kv_Store_open`, `libkvstore.so`, and an npm, PyPI,
or NuGet package named `kvstore`). Nothing a generator emits is named after
WeaveFFI, so any number of WeaveFFI-built libraries can live in one process.

**A small, explicit C ABI.** [Revision 5](reference/abi.md) passes every
value in one of a few families: direct scalars, optional scalars as a
presence flag plus the value, numeric lists as typed arrays, UTF-8 strings
and bytes as `(ptr, len)` runs, serialized
[value buffers](reference/value-buffers.md) for records, rich enums, and the
remaining optionals, lists, and maps, and reference-counted objects, plus
consumer-implemented callback vtables. Every fallible call reports through a
`{prefix}_error` struct carrying a code, a length-delimited message, and the
code's fields. Async functions complete through a callback that fires
exactly once, and cancellation is a first-class runtime code. Every consumer
checks the ABI revision and each module's contract table (a hash of every
declaration's signature, error code, and callback method) when it loads the
library, so a stale binding fails loudly, naming the declaration that
changed, instead of misreading memory, while a library that only added
declarations, codes, or callback methods keeps loading.

**Idiomatic generators.** Each target maps the model onto its own idioms:
objects become classes with deterministic disposal and a garbage-collector
backstop, async functions become `async`/`await`, coroutines, promises, or
tasks with native cancellation, iterators stay lazy, and error domains
become typed exceptions or error values. The
[capability matrix](generators/README.md) summarizes all eleven: C, C++,
Swift, Kotlin, Python, Node.js, and .NET are Tier 1, and Go, Ruby, Dart, and
WebAssembly are Tier 2 (see [Stability](stability.md#target-tiers)).

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
- [Stability and Versioning](stability.md): what the version numbers
  promise, and how to migrate to schema 0.12 and ABI revision 5.
