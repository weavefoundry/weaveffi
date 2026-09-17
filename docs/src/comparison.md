# Comparison

WeaveFFI sits in a crowded ecosystem of FFI tooling. This page is an honest,
side-by-side look at how it compares to the projects you are most likely to
evaluate against it: **UniFFI** (Mozilla's multi-language generator),
**Diplomat** (the Rust-to-many-languages tool behind ICU4X), **cbindgen** plus
hand-written glue, the single-language bridges **swift-bridge**, **napi-rs**,
and **wasm-bindgen**, and the C/C++-first generators **SWIG** and **autocxx**.

> All comparisons reflect the public state of each project at the time of
> writing (ABI revision 2, schema `0.9.0`). If something here is out of date,
> please open a PR.

## The short version

WeaveFFI's distinguishing bet is *one language-neutral IDL, one C ABI, eleven
generators*. The producer can be Rust (with `#[weaveffi::module]` writing the
ABI for you) or anything else that can export C symbols, and every generated
package is standalone: consumers never install WeaveFFI. Since ABI revision 2
the object model is on par with the Rust-first tools: interface objects are
reference counted (`Arc<T>`), can be shared between wrappers and nested inside
records, lists, maps, optionals, iterators, and async results, and callback
interfaces let the producer call consumer code through a vtable.

The honest flip side is that WeaveFFI is pre-1.0 and its type system is
deliberately smaller than UniFFI's: no user-defined generics or trait objects
with generics, callback methods return only scalars (no strings, buffers, or
objects yet), no async callback methods, and no multi-file IDL imports. The
[roadmap](roadmap.md) lists what's planned.

## At a glance

|                                    | **WeaveFFI** | **UniFFI** | **Diplomat** | **cbindgen + glue** | **swift-bridge** | **napi-rs** | **wasm-bindgen** | **SWIG** | **autocxx** |
|------------------------------------|:------------:|:----------:|:------------:|:-------------------:|:----------------:|:-----------:|:----------------:|:--------:|:-----------:|
| Producer language                  | Rust, C, C++, Zig (anything with a C ABI) | Rust | Rust | Rust | Rust | Rust | Rust | C / C++ | C++ (consumed from Rust) |
| Input                              | YAML / JSON / TOML IDL or annotated Rust | UDL or proc-macros | annotated Rust bridge crate | Rust source | annotated Rust bridge module | annotated Rust | annotated Rust | C/C++ headers + `.i` file | C++ headers |
| Consumer languages                 | C, C++, Swift, Kotlin, Node.js, Wasm/JS, Python, .NET, Dart, Go, Ruby | Kotlin, Swift, Python, Ruby first-party; Go, C#, Dart, Kotlin Multiplatform, React Native as external bindgens | C, C++, JS/TS (Wasm), Dart, Kotlin (JNA), Python (nanobind) | C (you write the rest) | Swift | Node.js | JS/TS (Wasm) | many (Python, Java, C#, Ruby, Lua, Perl, PHP, R, ...) | Rust |
| Standalone C header                | ✓ (the contract every target shares) | ✗ (private scaffolding ABI) | ✓ (C backend) | ✓ (its purpose) | ✓ (generated) | ✗ | ✗ | ✗ | n/a |
| **Type system**                    |              |            |              |                     |                  |             |                  |          |             |
| Records / enums / optionals / lists / maps | ✓ (value buffers) | ✓ | ✓ (structs, enums, `Option`, slices; no maps) | manual | ✓ (transparent structs and enums, `Option`, `Vec`) | ✓ (serde objects) | ✓ (`serde-wasm-bindgen` or classes) | ✓ | ✓ |
| Objects with methods               | ✓ reference counted, shareable, nestable | ✓ (`Arc<T>`) | ✓ (opaques, borrowed or owned) | manual | ✓ (opaque types) | ✓ (`#[napi]` classes) | ✓ (classes) | ✓ (classes) | ✓ (C++ classes) |
| Callback interfaces                | ✓ (vtable; sync, scalar returns) | ✓ (foreign traits; any return, `throws`, async) | partial (Kotlin, C, C++; input-only) | manual fn pointers | partial (closures Rust to Swift) | ✓ (`ThreadsafeFunction`) | ✓ (closures) | partial (directors) | partial |
| Typed error domains                | ✓ (per-module codes, opt-in `throws`, payload fields) | ✓ (error enums) | ✓ (`Result`) | manual | ✓ (`Result`) | ✓ (JS `Error`) | ✓ (`Result<JsValue>`) | ✗ | ✗ |
| Async functions                    | ✓ (callback ABI, pluggable spawner, cancel tokens) | ✓ (poll-based, foreign executors) | ✗ | manual | ✓ (both directions) | ✓ (Tokio) | ✓ (`Promise`) | ✗ | ✗ |
| Iterators                          | ✓ (`iter<T>`, lazy on every target) | ✗ (materialize or use a trait) | partial (`DiplomatWrite`) | manual | ✗ | ✗ | ✓ (JS iterators) | partial | ✗ |
| Generics / trait objects           | ✗ (fixed set of built-in shapes) | partial (traits, no generics) | partial (traits on some backends) | ✗ | partial | ✗ | ✗ | ✓ (templates via `%template`) | ✓ |
| Multi-file / multi-crate definitions | ✗ (one document per API) | ✓ (external types across crates) | ✓ (one bridge crate, many modules) | n/a | ✗ | n/a | n/a | ✓ (`%include`) | ✓ |
| **Workflow**                       |              |            |              |                     |                  |             |                  |          |             |
| Standalone CLI                     | ✓ (`cargo install weaveffi-cli`) | `uniffi-bindgen` (build.rs or CLI) | `diplomat-tool` | ✓ | `swift-bridge-cli` | `napi` CLI (npm) | `wasm-bindgen-cli` | system package | cargo build |
| Publishable per-ecosystem packages | ✓ (`weaveffi package` for every target) | partial | partial | n/a | ✓ (SwiftPM) | ✓ (npm) | ✓ (npm via wasm-pack) | ✗ | n/a |
| Schema-checked IDL with JSON Schema | ✓ | ✗ | n/a | n/a | n/a | n/a | n/a | ✗ | n/a |
| Load-time ABI version check        | ✓ (`weaveffi_abi_version`) | ✓ (checksums) | ✗ | ✗ | ✗ | ✓ (N-API version) | ✗ | ✗ | n/a |
| Generated-output drift check in CI | ✓ (`weaveffi diff --check`) | build-time | build-time | ✓ | build-time | build-time | build-time | ✗ | build-time |
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
  battle-testing across iOS, Android, and desktop. Its foreign traits can
  return any type (strings, records, objects), can `throw`, and can be
  `async`; WeaveFFI's callback-interface methods are synchronous and return
  only scalars, `bool`, or a C-style enum. UniFFI also supports external types
  across crates, whereas a WeaveFFI API is one IDL document (or one annotated
  Rust source tree). If your matrix is Kotlin, Swift, and Python and you want
  maximum maturity, UniFFI is the safer pick.
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
  `wasm32-unknown-unknown` build has no threads, so async functions resolve
  inline and callback-interface methods fire only while a call into the
  module is on the stack; a producer that calls back from a spawned thread
  can't run there. wasm-bindgen's `wasm-bindgen-futures` integrates with the
  JS event loop natively, and Emscripten-based toolchains can use
  `pthread`s. In WeaveFFI's Emscripten compatibility mode, async functions
  and callback interfaces are not available at all.
- **No formal stability guarantee yet.** WeaveFFI is pre-1.0; schema `0.9.0`
  and ABI revision 2 removed constructs without compatibility shims (see the
  [migration guide](stability.md#migrating-from-schema-080--abi-1-to-090--abi-2)).
  UniFFI, cbindgen, napi-rs, wasm-bindgen, and SWIG offer stronger
  compatibility commitments today.

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
   call from any thread, and a consumer-side exception surfaces to the
   original caller as a typed foreign error.
5. **Standalone, publishable consumer packages.** Generated packages are
   self-contained; `weaveffi package` bundles the native library per platform
   (including `jniLibs/<abi>/` for Kotlin and the `.wasm` for npm). There is no
   "install WeaveFFI" step on the consumer side.
6. **Idiomatic per-target output, not a lowest-common-denominator API.**
   Async functions become `async/await` in Swift, `Promise`s in Node and
   Wasm, `suspend fun` in Kotlin, `async def` in Python, `Task<T>` in C#, and
   `Future<T>` in Dart, all from the same `async: true` flag; Rust producers
   can plug their own executor with `weaveffi::set_spawner`.
7. **A CI-first CLI.** `validate`, `diff --check`, `extract`, `schema`, and
   `package` are designed to drop into pipelines, every generator's output is
   byte-for-byte deterministic, and generated consumers refuse to load a
   producer built for a different ABI revision.

## When to choose something else

- **You only need Kotlin, Swift, and Python and want maximum stability, or
  you need callback methods that return strings, records, or objects, or
  async callbacks**: use UniFFI.
- **You only need a C header for a Rust crate**: use cbindgen.
- **You ship to exactly one of Swift, Node.js, or the browser**: use
  swift-bridge, napi-rs, or wasm-bindgen respectively.
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
to WeaveFFI converter today, but `weaveffi extract` can read annotated Rust
source and produce a starting IDL, which is often the fastest path off any
Rust-only generator. See the [extract guide](guides/extract.md) for details,
and the [C ABI contract](reference/abi.md) if you're bringing a non-Rust
producer to the generated header.
