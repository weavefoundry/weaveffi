# Introduction

**WeaveFFI generates idiomatic bindings for 11 languages from one API
definition, over a stable C ABI.** Write the API as safe Rust tagged with
WeaveFFI's attributes, or as a YAML or JSON IDL implemented by any library
that can export C symbols (C, C++, Zig, and so on). WeaveFFI emits packages
for C, C++, Swift, Kotlin, Node.js, WebAssembly, Python, .NET, Dart, Go, and
Ruby that all call the same C functions.

## The design in one page

**One definition, two front doors.** A Rust producer tags ordinary items
(`#[weaveffi::export]` on a function or an `impl` block,
`#[weaveffi::record]` on a struct, and so on). Each attribute emits the
item's `extern "C"` functions and embeds a description of the item in the
compiled library, and the CLI reads that description back out to emit the
bindings, so the bindings describe exactly the library you compiled,
`#[cfg]` and Cargo features included. A non-Rust producer writes an
[IDL](reference/idl.md) instead and implements the C header WeaveFFI
generates from it.

**One library, one namespace.** Every record, enum, error domain,
interface, callback interface, and function has a name unique across the
library, and every C symbol is the library's prefix plus that name
(`kvstore_Store_open`). An IDL may still be split across files with
`imports:`, and a Rust crate across modules; neither changes a name.

**Identity drives every name.** A library has one resolved
[identity](reference/naming.md): a package `name`, a C symbol `prefix`, and a
native `library` base name. For a Rust producer all three come from the crate
(`kvstore` gives `kvstore_Store_open`, `libkvstore.so`, and an npm, PyPI, or
NuGet package named `kvstore`). Nothing a generator emits is named after
WeaveFFI, so any number of WeaveFFI-built libraries can live in one process.

**A small, explicit C ABI.** In [revision 6](reference/abi.md) every value
crosses as exactly one C value, in every position: a scalar by value, an
optional scalar as a `{p}_opt_*` struct, an object as a pointer, and
everything else (strings, bytes, typed arrays of numbers, and the
serialized [value buffers](reference/value-buffers.md) that carry records,
rich enums, and the remaining optionals, lists, and maps) as one
`{p}_run`, a pointer and a byte length. A callback interface is one
`{ctx, vtable}` struct. Every fallible call reports through a `{p}_error`
struct carrying a code, a length-delimited message, and the code's fields.
Async functions complete through a callback that fires exactly once, and
cancellation is a first-class runtime code.

**Bindings that refuse to drift.** Every declaration exports a contract
function returning a hash of its signature. Each consumer checks the ABI
revision and every declaration it was generated with when it loads the
library, so a stale binding fails loudly, naming the declaration that
changed, instead of misreading memory, while a library that only added
declarations, error codes, or callback methods keeps loading.

**Idiomatic generators.** Each target maps the model onto its own idioms:
objects become classes with deterministic disposal and a garbage-collector
backstop, async functions become `async`/`await`, coroutines, promises, or
tasks with native cancellation, iterators stay lazy, and error domains
become typed exceptions or error values. The
[capability matrix](generators/README.md) summarizes all eleven.

**Standalone output.** Generated packages are self-contained: helper code
(the value-buffer codec, error types, object wrappers, loaders) is generated
into each package, and no target needs compiled glue of its own, so
consumers never install WeaveFFI. `weaveffi package` bundles prebuilt native
libraries into publishable packages per ecosystem.

## Where to next

- [Getting Started](getting-started.md): a Rust producer called from C and
  Python in a few minutes, plus the IDL path for a C implementation.
- [The Rust Producer Macros](guides/producer-macro.md): every attribute,
  with a compiled example of each.
- [C ABI Contract](reference/abi.md): the normative description of the
  boundary.
- [Comparison](comparison.md) and [FAQ](faq.md): how WeaveFFI relates to
  UniFFI, Diplomat, cbindgen, and the single-language bridges.
- [Stability and Versioning](stability.md): what the version numbers
  promise.
