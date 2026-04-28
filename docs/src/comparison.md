# Comparison

WeaveFFI sits in a crowded ecosystem of FFI tooling. This page is an honest,
side-by-side look at how it compares to the projects you are most likely to
evaluate against it: **UniFFI**, **cbindgen**, **diplomat**, **SWIG**, and
**autocxx**.

> All comparisons reflect the public state of each project at the time of
> writing. If something here is out of date, please open a PR.

## At a glance

|                                    | **WeaveFFI** | **UniFFI** | **cbindgen** | **diplomat** | **SWIG** | **autocxx** |
|------------------------------------|:------------:|:----------:|:------------:|:------------:|:--------:|:-----------:|
| Source language                    | Rust / C / C++ / Zig (anything with a C ABI) | Rust | Rust | Rust | C / C++ | C++ |
| Input format                       | YAML / JSON / TOML IDL | UDL or proc-macro on Rust | Rust source (annotated) | Rust source (annotated) | C/C++ headers + `.i` interface | C++ headers |
| **Languages**                      |              |            |              |              |          |             |
| C                                  | ✓            | —          | ✓            | ✓            | ✓        | —           |
| C++                                | ✓ (RAII, `std::optional/vector/unordered_map`) | — | ✓ (header) | ✓            | ✓        | ✓ (its purpose) |
| Swift                              | ✓ (SwiftPM, `async/await`, `throws`) | ✓ | — | ✓ | — | — |
| Kotlin / Android (JNI)             | ✓ (Kotlin + JNI shim + Gradle) | ✓ | — | — | ✓ (Java via JNI) | — |
| Node.js                            | ✓ (N-API + `.d.ts`) | community add-on | — | — | ✓ (JavaScriptCore/V8) | — |
| WebAssembly                        | ✓ (loader + `.d.ts`) | — | — | ✓ (JS via WASM) | — | — |
| Python                             | ✓ (`ctypes` + `.pyi`) | ✓ | — | — | ✓ | — |
| .NET / C#                          | ✓ (P/Invoke + `.csproj`) | ✓ (community) | — | — | ✓ | — |
| Dart / Flutter                     | ✓ (`dart:ffi`)         | community | — | ✓ | — | — |
| Go                                 | ✓ (CGo)                 | community | — | — | ✓ | — |
| Ruby                               | ✓ (FFI gem)             | — | — | — | ✓ | — |
| **Type system**                    |              |            |              |              |          |             |
| Primitives + `string`              | ✓            | ✓          | ✓            | ✓            | ✓        | ✓           |
| `bytes` / byte slices              | ✓            | ✓          | ✓ (raw)      | ✓            | partial  | ✓           |
| Structs                            | ✓ (opaque + getters) | ✓ (records & objects) | ✓ (`#[repr(C)]`) | ✓ (opaque) | ✓ | ✓ |
| Enums w/ explicit discriminants    | ✓            | ✓          | ✓            | ✓            | ✓        | ✓           |
| Optionals                          | ✓ (`T?`)     | ✓          | partial      | ✓            | partial  | ✓           |
| Lists                              | ✓ (`[T]`)    | ✓          | partial      | ✓            | ✓        | ✓           |
| Maps                               | ✓ (`{K:V}`)  | ✓          | —            | ✓            | partial  | partial     |
| Typed handles (`handle<T>`)        | ✓            | ✓ (objects) | —          | ✓ (opaque)   | partial  | —           |
| Borrowed types (`&str`, `&[u8]`)   | ✓            | partial    | ✓            | ✓            | —        | ✓           |
| Iterators (`iter<T>`)              | ✓            | ✓ (callbacks) | —         | partial      | partial  | —           |
| Async functions                    | ✓ (callback ABI + `async/await`/`Promise`/`suspend`/`Task<T>`) | ✓ | — | partial | — | — |
| Cancellable futures                | ✓ (`weaveffi_cancel_token`) | partial | — | — | — | — |
| Callbacks / event listeners        | ✓ (module-level) | ✓     | — (raw fn ptrs) | partial   | partial  | partial     |
| Cross-module type references       | ✓            | ✓          | n/a          | ✓            | ✓        | ✓           |
| Nested modules                     | ✓            | partial    | n/a          | ✓            | ✓        | ✓           |
| **Workflow**                       |              |            |              |              |          |             |
| Single-binary CLI install          | ✓ (`cargo install weaveffi-cli`) | ✓ | ✓ | ✓ | system package | ✓ |
| Standalone publishable packages    | ✓ (npm, SwiftPM, pub.dev, NuGet, gem, etc.) | partial | n/a | partial | partial | n/a |
| JSON Schema for IDL editor support | ✓            | —          | n/a          | n/a          | —        | n/a         |
| `extract` from annotated source    | ✓ (Rust)     | ✓ (proc-macro) | ✓ (Rust) | ✓ (Rust)    | n/a      | ✓ (C++)     |
| `watch` mode                       | ✓            | —          | ✓ (`--watch`) | —          | —        | partial     |
| `format` IDL canonicalizer         | ✓            | —          | n/a          | n/a          | —        | n/a         |
| Schema migrations (`upgrade`)      | ✓            | —          | n/a          | n/a          | —        | n/a         |
| Custom template overrides (Tera)   | ✓            | partial (Mako) | —        | partial      | ✓ (`%typemap`) | partial |
| Snapshot-tested generator output   | ✓            | ✓          | ✓            | ✓            | partial  | ✓           |
| Maturity                           | pre-1.0      | 1.0+ in Mozilla shipping products | 1.0+ widely deployed | pre-1.0 | 30+ years, ubiquitous | pre-1.0 |
| License                            | MIT OR Apache-2.0 | MPL-2.0 | MPL-2.0 | BSD-3-Clause | GPL with FOSS exception | MIT OR Apache-2.0 |

Legend: ✓ = first-class support; *partial* = supported with caveats or via
extensions; — = not supported; *n/a* = not applicable to that tool's scope.

## Where competitors are stronger

We try hard to be honest about the trade-offs. Pick the right tool for the job:

- **UniFFI is more mature.** It ships in production at Mozilla (Firefox Sync,
  Glean, Nimbus) and has years of battle-testing across iOS, Android, and
  desktop. If you only need Swift, Kotlin, and Python today and you are
  comfortable with a UDL-or-proc-macro workflow, UniFFI is the safer choice.
- **cbindgen is simpler if all you want is a C header.** WeaveFFI generates
  a C header *and* ten other targets — if you only consume the C surface
  from C/C++ code, cbindgen has less ceremony, no IDL file, and a smaller
  dependency footprint.
- **diplomat has a more polished C++ story.** Its C++ output uses richer
  templates and integrates more cleanly with existing C++ codebases. WeaveFFI's
  C++ output is RAII-based and includes a `CMakeLists.txt`, but it's
  optimized for greenfield projects, not for slotting into a 20-year-old
  C++ build system.
- **SWIG covers languages WeaveFFI doesn't.** Lua, Tcl, R, Octave, Perl, PHP
  — if your target is exotic, SWIG probably has a generator. SWIG also
  natively understands C and C++ headers, so you don't need to author an
  IDL at all.
- **autocxx is unmatched for "wrap an existing C++ library."** It reads
  your C++ headers directly and uses bindgen + cxx under the hood. WeaveFFI
  does not parse C++; you describe the surface area you want to expose, and
  WeaveFFI generates the contract.
- **No IDE plugin yet.** The other tools listed have community VSCode/JetBrains
  extensions of varying quality. WeaveFFI ships a JSON Schema for editor
  autocompletion and a `format` command, but no first-party IDE plugin.
- **No formal stability guarantee yet.** WeaveFFI is pre-1.0; the IDL,
  generated output, and runtime symbol names can shift in minor releases
  (always with a `weaveffi upgrade` path). UniFFI, cbindgen, and SWIG offer
  stronger compatibility commitments today.

## When to choose WeaveFFI

WeaveFFI is the right pick when you want:

1. **One source of truth for many languages.** If your library has to land
   in npm *and* SwiftPM *and* PyPI *and* NuGet *and* pub.dev *and* RubyGems
   *and* a Go module *and* a Gradle artifact — that's the WeaveFFI sweet
   spot. UniFFI covers a smaller subset out of the box; cbindgen and
   autocxx don't try.
2. **Standalone, publishable consumer packages.** Generated packages are
   self-contained: a Swift consumer adds your `.xcframework` + a SwiftPM
   manifest and is done. No "install WeaveFFI" step on the consumer side.
3. **A native library that isn't (only) Rust.** WeaveFFI works against
   anything that exposes a stable C ABI — Rust (with `--scaffold`
   convenience), C, C++, Zig, etc. UniFFI and diplomat assume Rust;
   autocxx assumes C++.
4. **Idiomatic per-target output, not a lowest-common-denominator API.**
   Async functions become `async/await` in Swift, `Promise`s in Node,
   `suspend fun` in Kotlin, `async def` in Python, and `Task<T>` in C#
   — all from the same `async: true` flag in the IDL.
5. **A CLI workflow with `validate`, `lint`, `diff`, `watch`, `format`,
   and `upgrade`.** WeaveFFI is built for monorepos and CI: every
   sub-command has a `--format json` output mode, and `diff --check` and
   `format --check` are designed to drop into pre-commit and CI gates.
6. **Honest pre-1.0 churn that's mechanically migratable.** Every breaking
   IDL change ships with a `weaveffi upgrade` migration. You don't get
   stuck on an old version because the migration path is missing.

## When to choose something else

- **You only need Swift + Kotlin + Python and want maximum stability** —
  use UniFFI.
- **You only need a C header for a Rust crate** — use cbindgen.
- **You're wrapping a large existing C++ codebase** — use autocxx (or
  cxx + bindgen directly).
- **Your target language is Lua, Tcl, R, Octave, Perl, or PHP** — use SWIG.
- **You need a battle-tested C++ binding generator with rich template
  support** — use diplomat or SWIG.

## Migrating to / from WeaveFFI

WeaveFFI's IDL is intentionally close to UniFFI's UDL surface area, which
makes hand-porting straightforward in either direction. There is no
automatic UDL → WeaveFFI converter today, but `weaveffi extract` can read
annotated Rust source and produce a starting IDL, which is often the
fastest path off any Rust-only generator. See the
[extract guide](guides/extract.md) for details.
