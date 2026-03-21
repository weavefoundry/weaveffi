# WeaveFFI Roadmap

This roadmap tracks high-level goals for WeaveFFI. The project uses a Rust
workspace that generates multi-language bindings from a YAML-based IR,
exposing a stable C ABI consumed by language-specific wrappers.

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
- **Code generators** for C, Swift, Android, Node.js, and WASM targets
- **Calculator sample** demonstrating end-to-end usage (`add`, `mul`, `div`, `echo`)
- **C ABI layer** with error struct, string/bytes free functions, and handle convention

## Release goals

### 0.1.0 — MVP (current)

Deliver a usable CLI that reads a YAML IR, validates it, and generates
bindings for all five targets. Ship with the calculator sample and docs.

### 0.2.0 — IR expansion + packaging

- Extend the IR to support structs, enums, optional types, and arrays/slices
- Richer string and byte-buffer handling
- Packaging improvements (SwiftPM, Gradle, npm scaffolds)
- Better cross-compilation UX

### 0.3.0 — Annotated Rust input

- Support reading an annotated Rust crate as input (derive/proc-macro) instead of hand-written YAML
- Improved diagnostics and template customization hooks

### 0.4.0 — Safety + performance

- Zero-copy where safe, arena/pool patterns, lifetime-safe handles
- Incremental codegen and caching
- DX polish across all targets

### 0.5.0 — Ecosystem expansion

- Additional language targets (e.g., Python, .NET)
- Runtime libraries per target language (idiomatic error types, memory wrappers)
- Publishing to crates.io, npm, CocoaPods, Maven Central
- Prebuilt CLI binaries and release automation

## Non-goals (for now)

- **Async surface**: mapping callbacks to async/await or Promises is explicitly
  out of scope for the 0.1.x series. The IR rejects `async: true` at validation time.
- **Proc-macro input**: planned for 0.3.0, not before.
