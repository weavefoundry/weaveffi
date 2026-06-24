# Introduction

**WeaveFFI turns one safe-Rust definition into type-safe bindings for 11
languages: no hand-written JNI, no duplicate implementations, no unsafe
boilerplate.**

Annotate a normal Rust module with `#[weaveffi::module]` and the `weaveffi`
crate generates the stable C ABI for you; the same annotated source generates
idiomatic packages for C, C++, Swift, Kotlin/Android, Node.js, WebAssembly,
Python, .NET, Dart, Go, and Ruby. Prefer to design the contract first? Author
the IDL in YAML, JSON, or TOML instead. Both paths share one engine, so the
producer you build and the bindings you ship cannot drift.

WeaveFFI works with any native library that exposes the C ABI, whether it's
written in Rust, C, C++, Zig, or another language. Rust producers get the C
ABI for free from the [`#[weaveffi::module]` macro](guides/producer-macro.md);
other backends implement the symbols declared in the generated C header
directly.

## Why WeaveFFI?

- **One definition, eleven languages.** Write the API once (safe Rust or an
  IDL) and ship packages to npm, SwiftPM, Maven, PyPI, NuGet, pub.dev,
  RubyGems, and Go modules.
- **Safe Rust in, C ABI out.** The `#[weaveffi::module]` macro emits the
  `extern "C"` thunks, marshalling every argument through an audited runtime,
  so a Rust producer writes no `unsafe` glue and the IDL is derived from the
  code rather than maintained beside it.
- **Stable C ABI underneath.** Every target speaks to the same `extern "C"`
  contract, so adding a new platform later is a code-gen change, not a
  rewrite.
- **Idiomatic per-target output.** No lowest-common-denominator surface
  area. Swift gets `async/await` and `throws`, Kotlin gets `suspend` and
  JNI glue, Python gets typed `.pyi` stubs, TypeScript gets `Promise`s,
  Dart gets `dart:ffi`, all from the same definition.

## Design principle: standalone generated packages

Generated packages are fully self-contained and publishable to their
native ecosystem (npm, CocoaPods, Maven Central, PyPI, NuGet, pub.dev,
RubyGems, etc.) without requiring consumers to install WeaveFFI tooling
or runtime dependencies. WeaveFFI is a build-time tool for library
authors; consumers should never need to know it exists. Helper code
(error types, memory management utilities) is generated inline into each
package rather than pulled from a shared runtime dependency.

## Where to next

- [Getting Started](getting-started.md): install, annotate Rust, generate, and call from C.
- [The Rust Producer Macro](guides/producer-macro.md): the `#[weaveffi::module]` attribute family, the supported feature set, and the roadmap.
- [Comparison](comparison.md): feature matrix vs UniFFI, cbindgen, diplomat, SWIG, autocxx, and an honest "when to choose WeaveFFI" guide.
- [FAQ](faq.md): runtime cost, customization, Windows support, distribution, licensing.
- [Samples](samples.md): the kitchen-sink `kvstore` reference plus calculator/contacts/inventory walkthroughs.
- [Generators](generators/README.md): per-target reference for each of the eleven languages.
- [Guides](guides/README.md): memory ownership, error handling, async, configuration.
