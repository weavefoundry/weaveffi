# Introduction

**WeaveFFI generates type-safe bindings for 11 languages for any native library
that exposes a C ABI, whether it's written in Rust, C, C++, Zig, or another
language: no hand-written JNI, no duplicate implementations, no unsafe
boilerplate.**

Define your API once as an IDL in YAML, JSON, or TOML, and WeaveFFI generates
idiomatic packages for C, C++, Swift, Kotlin, Node.js, WebAssembly, Python,
.NET, Dart, Go, and Ruby, all talking to the same stable C ABI. Any backend
that implements the symbols declared in the generated C header can be the
producer.

Writing your producer in Rust? Annotate a normal module with
`#[weaveffi::module]` and the macro generates the C ABI and derives the IDL for
you, so you write no `unsafe` glue and keep no separate IDL in sync. The macro
is one ergonomic path onto the same engine the IDL uses, so whichever you pick,
the producer you build and the bindings you ship cannot drift.

## Why WeaveFFI?

- **One definition, eleven languages.** Write the API once (safe Rust or an
  IDL) and ship packages to npm, SwiftPM, Maven, PyPI, NuGet, pub.dev,
  RubyGems, and Go modules.
- **Safe Rust in, C ABI out.** The `#[weaveffi::module]` macro emits the
  `extern "C"` thunks, marshalling every argument through an audited runtime,
  so a Rust producer writes no `unsafe` glue and the IDL is derived from the
  code rather than maintained beside it.
- **Stable C ABI underneath.** Every target speaks to the same `extern "C"`
  contract ([ABI revision 2](reference/abi.md)), so adding a new platform
  later is a code-gen change, not a rewrite.
- **Objects and callbacks that behave the same everywhere.** Interfaces are
  reference-counted objects (`Arc<T>` in Rust) that consumers can share,
  nest inside records and collections, and release deterministically, with a
  garbage-collector backstop where the language has one. Callback interfaces
  are method sets the consumer implements as a protocol, interface, or
  abstract class and the native library calls from any thread.
- **Idiomatic per-target output.** No lowest-common-denominator surface
  area. Swift gets `async/await` and `throws`, Kotlin gets `suspend` and
  JNI glue, Python gets typed `.pyi` stubs, TypeScript gets `Promise`s and
  `BigInt`, Dart gets `dart:ffi` with `NativeFinalizer`, all from the same
  definition. Async producers keep their own executor: plug Tokio in with
  `weaveffi::set_spawner`, or let the default thread-per-future spawner do
  the work.

## Design principle: standalone generated packages

Generated packages are fully self-contained and publishable to their
native ecosystem (npm, SwiftPM, Maven Central, PyPI, NuGet, pub.dev,
RubyGems, and so on) without requiring consumers to install WeaveFFI tooling
or runtime dependencies. WeaveFFI is a build-time tool for library
authors; consumers should never need to know it exists. Helper code
(error types, the value-buffer codec, object wrappers, callback trampolines)
is generated inline into each package rather than pulled from a shared
runtime dependency.

## Where to next

- [Getting Started](getting-started.md): install, define an IDL, generate, and call from C.
- [The Rust Producer Macro](guides/producer-macro.md): the `#[weaveffi::module]` attribute family and the supported feature set.
- [C ABI Contract](reference/abi.md): the normative description of objects, callback interfaces, value buffers, async functions, and iterators at the boundary.
- [Comparison](comparison.md): feature matrix vs UniFFI, Diplomat, cbindgen, swift-bridge, napi-rs, wasm-bindgen, SWIG, autocxx, and an honest "when to choose WeaveFFI" guide.
- [FAQ](faq.md): object ownership, callback interfaces, the async spawner, runtime cost, customization, Windows support, distribution, licensing.
- [Samples](samples.md): the kitchen-sink `kvstore` reference, the `events` callback-interface bus, the `codec` round-trip oracle, and the smaller walkthroughs.
- [Stability and Versioning](stability.md): what schema `0.9.0` and ABI 2 changed and how to migrate.
- [Roadmap](roadmap.md): what's planned after ABI 2.
- [Generators](generators/README.md): per-target reference for each of the eleven languages.
- [Guides](guides/README.md): memory ownership, error handling, async, configuration, packaging.
