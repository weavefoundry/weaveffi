# Introduction

**WeaveFFI generates type-safe bindings for 11 languages from a single IDL —
no hand-written JNI, no duplicate implementations, no unsafe boilerplate.**

Define your API once in YAML, JSON, or TOML; ship idiomatic packages for C,
C++, Swift, Kotlin/Android, Node.js, WebAssembly, Python, .NET, Dart, Go,
and Ruby that all talk to the same stable C ABI.

WeaveFFI works with any native library that exposes a stable C ABI —
whether it's written in Rust, C, C++, Zig, or another language. Rust gets
first-class scaffolding via `weaveffi generate --scaffold`; other backends
implement the symbols declared in the generated C header directly.

## Why WeaveFFI?

- **One IDL, eleven languages.** Describe your API once and ship packages
  to npm, SwiftPM, Maven, PyPI, NuGet, pub.dev, RubyGems, and Go modules.
- **Stable C ABI underneath.** Every target speaks to the same `extern "C"`
  contract, so adding a new platform later is a code-gen change, not a
  rewrite.
- **Idiomatic per-target output.** No lowest-common-denominator surface
  area. Swift gets `async/await` and `throws`, Kotlin gets `suspend` and
  JNI glue, Python gets typed `.pyi` stubs, TypeScript gets `Promise`s,
  Dart gets `dart:ffi` — all from the same definition.

## Design principle: standalone generated packages

Generated packages are fully self-contained and publishable to their
native ecosystem (npm, CocoaPods, Maven Central, PyPI, NuGet, pub.dev,
RubyGems, etc.) without requiring consumers to install WeaveFFI tooling
or runtime dependencies. WeaveFFI is a build-time tool for library
authors — consumers should never need to know it exists. Helper code
(error types, memory management utilities) is generated inline into each
package rather than pulled from a shared runtime dependency.

## Where to next

- [Getting Started](getting-started.md) — install → IDL → generate → call from C.
- [Comparison](comparison.md) — feature matrix vs UniFFI, cbindgen, diplomat, SWIG, autocxx, and an honest "when to choose WeaveFFI" guide.
- [FAQ](faq.md) — runtime cost, customization, Windows support, distribution, licensing.
- [Samples](samples.md) — the kitchen-sink `kvstore` reference plus calculator/contacts/inventory walkthroughs.
- [Generators](generators/README.md) — per-target reference for each of the eleven languages.
- [Guides](guides/README.md) — memory ownership, error handling, async, configuration.
