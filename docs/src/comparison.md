# Comparison

WeaveFFI sits in a crowded ecosystem of FFI tooling. This page is an honest,
side-by-side look at how it compares to the projects you are most likely to
evaluate against it: **UniFFI** (Mozilla's multi-language generator),
**Diplomat** (the Rust-to-many-languages tool behind ICU4X), **cbindgen** plus
hand-written glue, the single-language bridges **swift-bridge**, **napi-rs**,
and **wasm-bindgen**, and the C/C++-first generators **SWIG** and **autocxx**.
A [shorter section](#other-tools) covers **interoptopus**, **flapigen**,
**PyO3** with **maturin**, and **cxx**.

> All comparisons reflect the public state of each project at the time of
> writing (WeaveFFI ABI revision 5, schema `0.12.0`). If something here is out
> of date, please open a PR.

## The short version

WeaveFFI's distinguishing bet is *one language-neutral IDL, one C ABI, eleven
generators*. The producer can be Rust (with `#[weaveffi::module]` writing the
ABI for you) or anything else that can export C symbols, and every generated
package is standalone: consumers never install WeaveFFI. The object model is
on par with the Rust-first tools: interface objects are reference counted
(`Arc<T>`), can be shared between wrappers and nested inside records, lists,
maps, optionals, iterators, and async results, and callback interfaces let
the producer call consumer code through a vtable whose methods can return any
value and raise typed errors. A Rust producer's API is read from the library
it compiled (library mode, as in UniFFI), and `weaveffi package` turns the
builds into installable artifacts for every ecosystem. Every symbol and
package is named after the library, never after WeaveFFI, so several
WeaveFFI-built libraries coexist in one process.

The honest flip side is that WeaveFFI is pre-1.0 and its type system is
deliberately smaller than UniFFI's: no user-defined generics or trait
objects, custom types only on the producer side (bindings see the type's
builtin representation), no async callback methods, and no multi-file IDL
imports. The [roadmap](roadmap.md) lists what's planned.

## At a glance

|                                    | **WeaveFFI** | **UniFFI** | **Diplomat** | **cbindgen + glue** | **swift-bridge** | **napi-rs** | **wasm-bindgen** | **SWIG** | **autocxx** |
|------------------------------------|:------------:|:----------:|:------------:|:-------------------:|:----------------:|:-----------:|:----------------:|:--------:|:-----------:|
| Producer language                  | Rust, C, C++, Zig (anything with a C ABI) | Rust | Rust | Rust | Rust | Rust | Rust | C / C++ | C++ (consumed from Rust) |
| Input                              | YAML / JSON IDL or annotated Rust (read from the built library) | UDL or proc-macros (library mode reads the built library) | annotated Rust bridge crate | Rust source | annotated Rust bridge module | annotated Rust | annotated Rust | C/C++ headers + `.i` file | C++ headers |
| Consumer languages                 | C, C++, Swift, Kotlin, Node.js, Wasm/JS, Python, .NET, Dart, Go, Ruby | Kotlin, Swift, Python, Ruby first-party; Go, C#, Dart, Kotlin Multiplatform, React Native (`uniffi-bindgen-react-native`) as external bindgens | C, C++, JS/TS (Wasm), Dart, Kotlin (JNA), Python (nanobind) | C (you write the rest) | Swift | Node.js | JS/TS (Wasm) | many (Python, Java, C#, Ruby, Lua, Perl, PHP, R, ...) | Rust |
| Standalone C header                | ✓ (the contract every target shares) | ✗ (private scaffolding ABI) | ✓ (C backend) | ✓ (its purpose) | ✓ (generated) | ✗ | ✗ | ✗ | n/a |
| **Type system**                    |              |            |              |                     |                  |             |                  |          |             |
| Records / enums / optionals / lists / maps | ✓ (value buffers) | ✓ | ✓ (structs, enums, `Option`, slices; no maps) | manual | ✓ (transparent structs and enums, `Option`, `Vec`) | ✓ (serde objects) | ✓ (`serde-wasm-bindgen` or classes) | ✓ | ✓ |
| Objects with methods               | ✓ reference counted, shareable, nestable | ✓ (`Arc<T>`) | ✓ (opaques, borrowed or owned) | manual | ✓ (opaque types) | ✓ (`#[napi]` classes) | ✓ (classes) | ✓ (classes) | ✓ (C++ classes) |
| Callback interfaces                | ✓ (vtable; synchronous, any return, `throws`, optional, failures as `Result`) | ✓ (foreign traits; any return, `throws`, async) | partial (Kotlin, C, C++; input-only) | manual fn pointers | partial (closures Rust to Swift) | ✓ (`ThreadsafeFunction`) | ✓ (closures) | partial (directors) | partial |
| Typed error domains                | ✓ (several per module, open to new codes, payload fields; `throws` a domain or an untyped `any`) | ✓ (error enums) | ✓ (`Result`) | manual | ✓ (`Result`) | ✓ (JS `Error`) | ✓ (`Result<JsValue>`) | ✗ | ✗ |
| Async functions                    | ✓ (callback ABI, pluggable spawner, native cancellation on every target) | ✓ (poll-based, foreign executors) | ✗ | manual | ✓ (both directions) | ✓ (Tokio) | ✓ (`Promise`) | ✗ | ✗ |
| Iterators                          | ✓ (`iter<T>`, lazy on every target) | ✗ (materialize or use a trait) | partial (`DiplomatWrite`) | manual | ✗ | ✗ | ✓ (JS iterators) | partial | ✗ |
| Generics / trait objects           | ✗ (fixed set of built-in shapes) | partial (traits, no generics) | partial (traits on some backends) | ✗ | partial | ✗ | ✗ | ✓ (templates via `%template`) | ✓ |
| Multi-file / multi-crate definitions | ✗ (one document per API) | ✓ (external types across crates) | ✓ (one bridge crate, many modules) | n/a | ✗ | n/a | n/a | ✓ (`%include`) | ✓ |
| **Workflow**                       |              |            |              |                     |                  |             |                  |          |             |
| Standalone CLI                     | ✓ (`cargo install weaveffi-cli`) | `uniffi-bindgen` (build.rs or CLI) | `diplomat-tool` | ✓ | `swift-bridge-cli` | `napi` CLI (npm) | `wasm-bindgen-cli` | system package | cargo build |
| Publishable per-ecosystem packages | ✓ (`weaveffi package`: wheels, npm tarballs, gems, an XCFramework SwiftPM package, a `.nupkg`; Gradle, pub, and Go as ready-to-publish directories) | via companion tools (`cargo-swift`, `maturin`, `uniffi-bindgen-react-native`) | partial | n/a | ✓ (SwiftPM) | ✓ (npm) | ✓ (npm via wasm-pack) | ✗ | n/a |
| Schema-checked IDL with JSON Schema | ✓ | ✗ | n/a | n/a | n/a | n/a | n/a | ✗ | n/a |
| Load-time ABI check                | ✓ (ABI revision plus per-declaration contract tables) | ✓ (checksums) | ✗ | ✗ | ✗ | ✓ (N-API version) | ✗ | ✗ | n/a |
| Generated-output drift check in CI | ✓ (`weaveffi generate --check`) | build-time | build-time | ✓ | build-time | build-time | build-time | ✗ | build-time |
| Maturity                           | pre-1.0      | shipping in Firefox and Mozilla products since 2020 | shipping in ICU4X | 1.0+, widely deployed | 0.1.x, active | 2.x+, widely deployed | 0.2.x, ubiquitous | 30+ years | pre-1.0 |
| License                            | MIT OR Apache-2.0 | MPL-2.0 | MIT OR Apache-2.0 | MPL-2.0 | MIT OR Apache-2.0 | MIT | MIT OR Apache-2.0 | GPL-3.0 (generated code exempt) | MIT OR Apache-2.0 |

Legend: ✓ = first-class support; *partial* = supported with caveats, on a
subset of backends, or via extensions; ✗ = not supported; *manual* = you write
it by hand; *n/a* = not applicable to that tool's scope.

## Where competitors are stronger

Pick the right tool for the job. These are the places where another project
is ahead of WeaveFFI today.

- **UniFFI has the richer type system and more production mileage.** It
  ships in Firefox, Firefox Sync, Glean, and Nimbus and has years of
  battle-testing across iOS, Android, and desktop. Its foreign traits can be
  `async`; WeaveFFI's callback-interface methods can return any type and
  `throw`, but are synchronous. UniFFI's custom types can map to a
  consumer-side type in each language (WeaveFFI's are converted only on
  the producer side), and it supports external types across crates, whereas a
  WeaveFFI API is one IDL document (or one Rust producer crate). Its ecosystem of companion tools is larger
  too: `cargo-swift` builds the XCFramework and SwiftPM package, `maturin`
  builds Python wheels, and `uniffi-bindgen-react-native` targets React
  Native, where WeaveFFI's `weaveffi package` covers the first two itself
  and has no React Native target. If your matrix is Kotlin, Swift, and
  Python and you want maximum maturity, UniFFI is the safer pick.
- **Diplomat has the more mature Kotlin and JS story for a Rust library.**
  It powers ICU4X, its bridge-crate model keeps everything in Rust, and its
  C++ backend is polished for slotting into existing C++ builds. Its opaques
  support borrowed as well as owned references, which WeaveFFI does not
  model (every WeaveFFI object crossing is a strong reference). Diplomat has
  no async support and no maps, and callbacks exist only on some backends.
- **cbindgen is simpler if all you want is a C header.** WeaveFFI generates
  a C header *and* ten other targets. If you only consume the C surface from
  C or C++ code, cbindgen has less ceremony, no IDL, and a smaller footprint.
  You write the object lifecycle, error channel, and any callback plumbing
  yourself.
- **swift-bridge, napi-rs, and wasm-bindgen are deeper in their one
  language.** Each exposes idioms WeaveFFI's common denominator can't:
  swift-bridge bridges async functions in *both* directions and transparent
  Swift structs; napi-rs gives you the full N-API surface (`ThreadsafeFunction`,
  typed arrays, `AsyncTask`, class inheritance) and Tokio integration;
  wasm-bindgen talks to the whole Web platform through `web-sys` and
  `js-sys` and runs futures on the JS event loop. If you ship to exactly one
  of those ecosystems, the dedicated bridge is the better fit.
- **SWIG covers languages WeaveFFI doesn't.** Lua, Tcl, R, Octave, Perl, PHP,
  Java: if your target is exotic, SWIG probably has a generator, and it reads
  C and C++ headers directly so you author no IDL. It also handles C++
  templates.
- **autocxx is unmatched for "wrap an existing C++ library."** It reads your
  C++ headers and uses bindgen plus cxx under the hood. WeaveFFI does not
  parse C++; you describe the surface you want to expose and implement the
  generated header.
- **WeaveFFI's Wasm target is single-threaded.** The default
  `wasm32-unknown-unknown` build has no threads, so async functions are
  polled inline until they finish (a future that waits on something outside
  the call fails with `-1`, and there's little for an `AbortSignal` to
  cancel), and callback-interface methods fire only while a call into the
  module is on the stack. wasm-bindgen's `wasm-bindgen-futures` integrates
  with the JS event loop natively; WeaveFFI targets only
  `wasm32-unknown-unknown`, which has no threads.
- **The single-language tools package better for their one ecosystem.**
  `maturin` (with PyO3) and napi-rs's CLI build wheels and npm packages for
  every platform from one CI matrix with years of edge cases handled;
  `weaveffi package` is newer, and it doesn't prebuild the Node.js addon or
  the JNI shim for Windows.
- **No formal stability guarantee yet.** WeaveFFI is pre-1.0; schema
  `0.12.0` and ABI revision 5 changed the error struct, enum types, and the
  contract tables without compatibility shims (see the
  [migration guide](stability.md#migrating-from-025-to-the-next-release-schema-012-abi-5)).
  Revision 5 is designed to grow additively, but UniFFI, cbindgen, napi-rs,
  wasm-bindgen, and SWIG offer stronger compatibility commitments today.
- **Not every target is equally mature.** C, C++, Swift, Kotlin, Python,
  Node.js, and .NET are Tier 1; Go, Ruby, Dart, and WebAssembly are Tier 2
  and may lag new ABI work (see
  [Target tiers](stability.md#target-tiers)).

## When to choose WeaveFFI

WeaveFFI is the right pick when you want:

1. **One source of truth for many languages.** If your library has to land in
   npm *and* SwiftPM *and* PyPI *and* NuGet *and* pub.dev *and* RubyGems
   *and* a Go module *and* a Gradle artifact, that's the WeaveFFI sweet spot.
   UniFFI and Diplomat cover a smaller set out of the box; the
   single-language bridges don't try.
2. **A native library that isn't (only) Rust.** WeaveFFI works against
   anything that exposes a C ABI: Rust (with the `#[weaveffi::module]` macro
   generating the ABI for you), C, C++, Zig, and so on. UniFFI, Diplomat,
   swift-bridge, napi-rs, and wasm-bindgen assume Rust; autocxx assumes C++.
3. **Objects that behave the same everywhere.** Reference-counted interfaces
   with deterministic release (`close()`, `Dispose()`, `deinit`, RAII) and a
   garbage-collector backstop on every managed target, and the ability to put
   an object inside a record, a list, a map, an optional, an iterator, or an
   async result on all eleven targets, from one declaration.
4. **Callback interfaces without hand-written trampolines.** Declare the
   methods once and each target gets a protocol, interface, or abstract class
   to implement; the producer receives an `Arc<dyn Trait>` it can retain and
   call from any thread. Methods can return strings, records, and objects,
   and a consumer-side exception reaches the producer as a typed domain
   error (when the method throws one) or through `From<ForeignError>`, which
   it can propagate to the original caller with `?`.
5. **Standalone, publishable consumer packages.** Generated packages are
   self-contained; `weaveffi build` cross-compiles the producer per platform
   and `weaveffi package` bundles it into wheels, npm tarballs, gems, an
   XCFramework SwiftPM package, a NuGet package, and Gradle, pub, Go, and
   C/C++ distributions. There is no "install WeaveFFI" step on the consumer
   side.
6. **Idiomatic per-target output, not a lowest-common-denominator API.**
   Async functions become `async/await` in Swift, `Promise`s in Node and
   Wasm, `suspend fun` in Kotlin, `async def` in Python, `Task<T>` in C#, and
   `Future<T>` in Dart, all from the same `async: true` flag; Rust producers
   can plug their own executor with `weaveffi::set_spawner`. Cancelling the
   awaiting task, coroutine, or promise cancels the native call.
7. **A CI-first CLI.** `validate`, `generate --check`, `extract`, `schema`,
   `build`, and `package` are designed to drop into pipelines, every
   generator's output is
   byte-for-byte deterministic, and generated consumers refuse to load a
   producer built for a different ABI revision or from a different API
   definition (per-declaration contract tables, so adding a declaration
   never breaks a deployed binding).

## Other tools

- **interoptopus** generates C#, C, and Python bindings from annotated Rust
  (`#[ffi_function]`, `#[ffi_type]`), with patterns for services, slices,
  options, and callbacks. It's Rust-only on the producer side and covers
  fewer languages; its C# backend is especially polished.
- **flapigen** (formerly rust_swig) generates Java (JNI) and C++ bindings
  from a declarative `foreign_class!` description in the producer's build
  script. It's a good fit when exactly those two consumers matter.
- **PyO3 with maturin** builds a native CPython extension module from Rust:
  Python classes, exceptions, iterators, and async integration with direct
  access to the Python object model, and wheels for every platform. For a
  Python-only library it's deeper and better integrated than any
  multi-language tool, WeaveFFI's `ctypes` binding included.
- **cxx** is a safe, statically checked bridge between Rust and C++ in both
  directions (`#[cxx::bridge]`), sharing types like `String`, `Vec`, and
  `unique_ptr` without a C layer in between. If both sides are your own Rust
  and C++, it gives stronger guarantees than WeaveFFI's C++ target, which
  wraps a C ABI.

## When to choose something else

- **You only need Kotlin, Swift, and Python and want maximum stability, or
  you need async callbacks**: use UniFFI.
- **You only need a C header for a Rust crate**: use cbindgen.
- **You ship to exactly one of Swift, Node.js, Python, or the browser**: use
  swift-bridge, napi-rs, PyO3 with maturin, or wasm-bindgen respectively.
- **You need Rust and C++ calling each other in one codebase**: use cxx.
- **You need only C#, or only Java and C++**: interoptopus or flapigen may
  be enough.
- **You're wrapping a large existing C++ codebase from Rust**: use autocxx (or
  cxx plus bindgen directly).
- **Your target language is Lua, Tcl, R, Octave, Perl, or PHP**: use SWIG.
- **You want a Rust-only bridge crate with borrowed-reference semantics and a
  polished C++ backend**: use Diplomat (`#[diplomat::bridge]`, from the
  `rust-diplomat/diplomat` repository).

## Migrating to or from WeaveFFI

WeaveFFI's IDL is intentionally close to UniFFI's UDL surface area (records,
enums, interfaces, callback interfaces, error enums, `async`), which makes
hand-porting straightforward in either direction. There is no automatic UDL
to WeaveFFI converter today. Going the other way, `weaveffi extract` prints
the IDL a `#[weaveffi::module]` producer's built library embeds, which is the
starting point for reimplementing a Rust producer in another language. See
[Library Mode](guides/extract.md) for details, and the
[C ABI contract](reference/abi.md) if you're bringing a non-Rust producer to
the generated header.
