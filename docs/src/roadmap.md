# WeaveFFI Roadmap

This roadmap tracks high-level goals for WeaveFFI. The project generates
multi-language bindings from an API definition (YAML, JSON, or TOML), producing
a stable C ABI contract consumed by language-specific wrappers.

## Crate structure

| Crate | Purpose |
|---|---|
| `weaveffi-ir` | IR model + YAML/JSON/TOML parsing via `serde` |
| `weaveffi-abi` | C ABI runtime helpers (error struct, handles, memory free functions) |
| `weaveffi-core` | `Generator` trait, `Orchestrator`, validation, shared utilities |
| `weaveffi-gen-c` | C header generator |
| `weaveffi-gen-swift` | SwiftPM System Library + Swift wrapper generator |
| `weaveffi-gen-android` | Kotlin/JNI wrapper + Gradle skeleton generator |
| `weaveffi-gen-node` | N-API addon loader + TypeScript types generator |
| `weaveffi-gen-wasm` | Minimal WASM loader stub generator |
| `weaveffi-cli` | CLI binary (installed as `weaveffi`) |
| `samples/calculator` | End-to-end sample Rust library |
| `samples/node-addon` | N-API addon for the calculator sample |

## What works today

- **CLI** with three commands: `weaveffi generate`, `weaveffi new`, `weaveffi doctor`
- **IR parsing** from YAML with validation (name collisions, reserved keywords, unsupported shapes)
- **Code generators** for C, Swift, Android, Node.js, WASM, Python, and .NET targets
- **Map types** in the IR and all generators
- **Annotated Rust extraction** — derive/proc-macro input as an alternative to hand-written YAML
- **Incremental codegen** with content-hash caching to skip unchanged files
- **Generator configuration** via `[generators.<target>]` sections in the IDL
- **Calculator sample** demonstrating end-to-end usage (`add`, `mul`, `div`, `echo`)
- **C ABI layer** with error struct, string/bytes free functions, and handle convention

## Completed releases

### 0.1.0 — MVP

Delivered a usable CLI that reads a YAML IR, validates it, and generates
bindings for all five original targets. Shipped with the calculator sample
and docs.

### 0.2.0 — IR expansion + packaging

- Extended the IR to support structs, enums, optional types, arrays/slices, and maps
- Richer string and byte-buffer handling
- Packaging improvements (SwiftPM, Gradle, npm scaffolds)
- Better cross-compilation UX

### 0.3.0 — Annotated Rust input

- Annotated Rust crate extraction via derive/proc-macro as an alternative to hand-written YAML
- Improved diagnostics and template customization hooks

## Release goals

### 0.4.0 — Safety + performance (current)

- Zero-copy where safe, arena/pool patterns, lifetime-safe handles
- ~~Incremental codegen and caching~~ *(done)*
- DX polish across all targets

### 0.5.0 — Ecosystem expansion

- ~~Python target~~ *(done)*
- ~~.NET target~~ *(done)*
- Inline generated helpers per target (idiomatic error types, memory wrappers) — bundled into each package, not separate runtime dependencies
- Publishing to crates.io, npm, CocoaPods, Maven Central, PyPI, NuGet
- Prebuilt CLI binaries and release automation

### 0.6.0 — New horizons

- Async support investigation (currently rejected by the validator)
- C++ target
- Ruby target
- Dart/Flutter target

## Design principle: standalone generated packages

Generated packages should be fully self-contained and publishable to their
native ecosystem (npm, CocoaPods, Maven Central, etc.) without requiring
consumers to install WeaveFFI tooling, runtimes, or support packages.
WeaveFFI is a build-time tool for library authors — end users should never
need to know it exists. Any helper code (error types, memory management
utilities) is generated inline into each package rather than pulled from a
shared runtime dependency.

## Non-goals (for now)

- **Async surface**: mapping callbacks to async/await or Promises is explicitly
  out of scope. The IR rejects `async: true` at validation time. Investigation
  is planned for 0.6.0.
