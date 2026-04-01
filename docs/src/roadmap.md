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
| `weaveffi-gen-wasm` | WASM loader + JS/TS wrapper generator |
| `weaveffi-gen-python` | Python ctypes binding + `.pyi` stubs generator |
| `weaveffi-gen-dotnet` | .NET P/Invoke binding generator |
| `weaveffi-cli` | CLI binary (installed as `weaveffi`) |
| `samples/calculator` | End-to-end sample Rust library |
| `samples/contacts` | Contacts sample with structs, enums, and optionals |
| `samples/inventory` | Multi-module sample with cross-type features |
| `samples/node-addon` | N-API addon for the calculator sample |

## What works today

- **CLI** with commands: `generate`, `new`, `validate`, `extract`, `lint`, `diff`, `doctor`
- **IR parsing** from YAML, JSON, and TOML with validation (name collisions, reserved keywords, unsupported shapes)
- **Code generators** for C, Swift, Android, Node.js, WASM, Python, and .NET targets
- **Rich type system**: primitives, strings, bytes, handles, structs, enums, optionals, lists, and maps
- **Annotated Rust extraction** — derive/proc-macro input as an alternative to hand-written YAML
- **Incremental codegen** with content-hash caching to skip unchanged files
- **Generator configuration** via TOML config file with per-target options
- **Scaffolding** — `--scaffold` emits Rust `extern "C"` stubs for the API
- **Inline helpers** — error types and memory management utilities generated into each package
- **Samples** demonstrating end-to-end usage (calculator, contacts, inventory)
- **C ABI layer** with error struct, string/bytes free functions, error domains, and handle convention
- **Validation warnings** — `--warn` and `lint` command for non-fatal diagnostics
- **Diff mode** — compare generated output against existing files

## Completed

- [x] Usable CLI that reads a YAML IR, validates it, and generates bindings for all five original targets (C, Swift, Android, Node, WASM)
- [x] Calculator sample and mdBook documentation site
- [x] C ABI layer with error struct, string/bytes free functions, and handle convention
- [x] Extended IR: structs, enums, optional types, arrays/slices, and maps
- [x] Richer string and byte-buffer handling
- [x] Packaging improvements (SwiftPM, Gradle, npm scaffolds)
- [x] Annotated Rust crate extraction (`weaveffi extract`) as an alternative to hand-written YAML
- [x] Improved diagnostics, validation warnings, and `weaveffi lint` command
- [x] Incremental codegen with content-hash caching
- [x] Generator configuration via TOML config file
- [x] DX polish: `--dry-run`, `--quiet`, `--verbose`, `diff` command, improved `doctor`
- [x] Python target (ctypes + `.pyi` type stubs + pip-installable package)
- [x] .NET target (P/Invoke + `.csproj` + `.nuspec`)
- [x] Inline generated helpers per target (error types, memory wrappers)
- [x] WASM generator rewritten to be API-driven (JS wrappers + `.d.ts`)
- [x] Inventory sample demonstrating multi-module and cross-type features
- [x] Publishing to crates.io with automated semantic-release pipeline

## Future releases

### Quality and polish

- [ ] Align WASM generator with the C ABI error model (`out_err` parameter handling in generated JS)
- [ ] Complete Node N-API addon stub bodies (replace `// TODO: implement` with functional glue)
- [ ] Add end-to-end integration tests for JSON and TOML input formats
- [ ] Improve generator edge-case coverage (deeply nested optionals, maps of lists, enum-keyed maps)
- [ ] Fix stale validator error messages referencing old version strings
- [ ] Add `weaveffi completions <shell>` command for shell auto-completion (bash, zsh, fish, PowerShell)
- [ ] Improve `weaveffi new` to scaffold a complete working project (Cargo.toml, lib.rs, IDL, generated bindings)

### Safety and performance

- [ ] Zero-copy string and byte-buffer passing where safe (borrowed slices across the ABI boundary)
- [ ] Arena/pool allocation patterns for batch handle creation and destruction
- [ ] Typed handles replacing raw `u64` — each struct gets a distinct handle type to prevent misuse
- [ ] Add benchmarking infrastructure (criterion) for codegen throughput on large APIs
- [ ] Profile and optimize IR parsing and validation for APIs with hundreds of functions
- [ ] Audit generated code for memory safety (double-free, use-after-free, null pointer paths)

### C++ target

- [ ] C++ header generator with RAII wrapper classes for handles
- [ ] `std::string`, `std::vector`, `std::optional`, `std::unordered_map` type mappings
- [ ] Smart pointer wrappers (`std::unique_ptr` with custom deleters) for handle lifecycle
- [ ] CMakeLists.txt scaffold for consumer projects
- [ ] Exception-based error handling with typed exception classes from error domains
- [ ] Generator configuration for namespace, header guard style, and C++ standard version

### Template engine and extensibility

- [ ] User-overridable code templates per target (Tera or Handlebars-based)
- [ ] Template discovery from a `templates/` directory alongside the IDL file
- [ ] Pre-generation and post-generation hook commands (run arbitrary scripts before/after codegen)
- [ ] `[generators.<target>]` sections in the IDL file for inline per-target configuration
- [ ] IR schema version field with documented compatibility guarantees
- [ ] IR migration tool for upgrading IDL files between schema versions

### Async and callback support

- [ ] Remove the `async: true` validator rejection and design the async IR model
- [ ] Define the C ABI async convention (completion callbacks with context pointers)
- [ ] Swift `async/await` mapping for async functions
- [ ] Kotlin coroutine (`suspend fun`) mapping for async functions
- [ ] Node.js `Promise` mapping for async functions
- [ ] Python `asyncio` mapping for async functions
- [ ] .NET `Task<T>` / `async` mapping for async functions
- [ ] Cancellation token support for long-running async operations
- [ ] Async sample demonstrating cross-language async usage

### Dart and Flutter target

- [ ] `dart:ffi` bindings generator with Dart class wrappers for structs
- [ ] Dart enum generation with typed constructors
- [ ] Flutter plugin scaffold with platform channel integration
- [ ] `pubspec.yaml` and pub.dev packaging support
- [ ] Null-safety throughout generated Dart code
- [ ] Generator configuration for Dart package name and library prefix

### Go target

- [ ] CGo bindings generator with Go struct mappings
- [ ] Error handling via Go's `error` return pattern (no panics)
- [ ] `go.mod` and package layout generation
- [ ] Go-idiomatic naming (PascalCase exports, camelCase unexported)
- [ ] Slice and map conversions between Go and C memory
- [ ] Generator configuration for Go module path and package name

### Ruby target

- [ ] Ruby FFI bindings generator (using the `ffi` gem convention)
- [ ] Ruby class wrappers for structs with attribute accessors
- [ ] Gemspec scaffold for gem packaging and distribution
- [ ] Symbol-based enum mapping
- [ ] Memory management via custom `Fiddle::Pointer` release callbacks
- [ ] Generator configuration for gem name and module namespace

### Advanced patterns and IR evolution

- [ ] Streaming/iterator patterns in the IR (generator functions yielding sequences)
- [ ] Builder pattern support for complex struct construction
- [ ] Nested module support (sub-modules within modules)
- [ ] Callback/event listener patterns (register/unregister function pairs)
- [ ] Cross-module type references (struct in one module used as param in another)
- [ ] Const/immutable parameter annotations for safer codegen
- [ ] Versioned API evolution support (deprecated functions, added fields with defaults)

### Stability milestone

- [ ] Stability guarantees: IR schema, CLI interface, and Generator trait locked under SemVer
- [ ] Comprehensive migration guide for major version transitions
- [ ] Full cross-platform CI (add Windows to the test matrix)
- [ ] Security audit of all generated code patterns (memory safety, input validation)
- [ ] Published performance benchmarks for codegen throughput
- [ ] `weaveffi upgrade` command for migrating projects between versions
- [ ] Feature-complete documentation with per-target tutorials and cookbook recipes

## Design principle: standalone generated packages

Generated packages should be fully self-contained and publishable to their
native ecosystem (npm, CocoaPods, Maven Central, PyPI, NuGet, pub.dev, etc.)
without requiring consumers to install WeaveFFI tooling, runtimes, or support
packages. WeaveFFI is a build-time tool for library authors — end users should
never need to know it exists. Any helper code (error types, memory management
utilities) is generated inline into each package rather than pulled from a
shared runtime dependency.

## Non-goals (for now)

- **Full RPC / IPC framework**: WeaveFFI generates in-process FFI bindings, not
  network protocols. gRPC, Cap'n Proto, or similar tools are better suited for
  cross-process communication.
- **Automatic Rust implementation**: WeaveFFI generates the *consumer* side
  (bindings). The library author still writes the Rust (or C) implementation
  behind the ABI.
- **GUI framework bindings**: Complex GUI toolkits with deep object hierarchies
  and inheritance are out of scope. WeaveFFI targets function-oriented APIs with
  flat or moderately nested data types.
