# WeaveFFI Roadmap

This roadmap tracks high-level goals for WeaveFFI. The project generates
multi-language bindings from an API definition (YAML, JSON, or TOML), producing
a stable C ABI contract consumed by language-specific wrappers.

## Crate structure

| Crate | Purpose |
|---|---|
| `weaveffi-ir` | IR model + YAML/JSON/TOML parsing via `serde` |
| `weaveffi-abi` | C ABI runtime helpers (error struct, handles, memory free functions) |
| `weaveffi-core` | `Generator` trait, `Orchestrator`, validation, shared utilities, template engine |
| `weaveffi-gen-c` | C header generator |
| `weaveffi-gen-cpp` | C++ header + RAII wrapper + CMake scaffold generator |
| `weaveffi-gen-swift` | SwiftPM System Library + Swift wrapper generator |
| `weaveffi-gen-android` | Kotlin/JNI wrapper + Gradle skeleton generator |
| `weaveffi-gen-node` | N-API addon loader + TypeScript types generator |
| `weaveffi-gen-wasm` | WASM loader + JS/TS wrapper generator |
| `weaveffi-gen-python` | Python ctypes binding + `.pyi` stubs generator |
| `weaveffi-gen-dotnet` | .NET P/Invoke binding generator |
| `weaveffi-gen-dart` | Dart `dart:ffi` binding + `pubspec.yaml` generator |
| `weaveffi-gen-go` | Go CGo binding + `go.mod` generator |
| `weaveffi-gen-ruby` | Ruby FFI binding + gemspec generator |
| `weaveffi-cli` | CLI binary (installed as `weaveffi`) |
| `samples/calculator` | End-to-end sample Rust library |
| `samples/contacts` | Contacts sample with structs, enums, and optionals |
| `samples/inventory` | Multi-module sample with cross-type features |
| `samples/node-addon` | N-API addon for the calculator sample |
| `samples/async-demo` | Async demo with callback-based C ABI convention |
| `samples/events` | Events sample with callbacks, listeners, and iterators |

## What works today

- **CLI** with commands: `generate`, `new`, `validate`, `extract`, `lint`, `diff`, `doctor`, `upgrade`, `completions`, `schema-version`
- **IR parsing** from YAML, JSON, and TOML with validation (name collisions, reserved keywords, unsupported shapes)
- **Code generators** for C, C++, Swift, Android, Node.js, WASM, Python, .NET, Dart, Go, and Ruby targets
- **Rich type system**: primitives, strings, bytes, handles, typed handles, structs, enums, optionals, lists, maps, iterators, callbacks
- **Annotated Rust extraction** — derive/proc-macro input as an alternative to hand-written YAML
- **Incremental codegen** with content-hash caching to skip unchanged files
- **Generator configuration** via TOML config file with per-target options
- **Inline generator config** via `[generators.<target>]` sections in IDL files
- **Template engine** — Tera-based user-overridable code templates loaded from a `templates/` directory
- **Pre/post hooks** — run arbitrary scripts before and after code generation
- **Scaffolding** — `--scaffold` emits Rust `extern "C"` stubs for the API (sync and async)
- **Inline helpers** — error types and memory management utilities generated into each package
- **Samples** demonstrating end-to-end usage (calculator, contacts, inventory, async-demo, events)
- **C ABI layer** with error struct, string/bytes free functions, error domains, typed handles, and callback convention
- **Validation warnings** — `--warn` and `lint` command for non-fatal diagnostics
- **Diff mode** — compare generated output against existing files
- **Shell completions** — `weaveffi completions <shell>` for bash, zsh, fish, PowerShell
- **Schema versioning** — IR version field with `weaveffi upgrade` for migration and `schema-version` for querying
- **Async IR model** — async functions with completion callback convention and cancellation support
- **Advanced IR features** — sub-modules, builder pattern, deprecated/since annotations, mutable params, default field values, borrowed types

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
- [x] C++ target (RAII wrappers, `std::string`/`std::vector`/`std::optional`/`std::unordered_map`, CMakeLists.txt, exception-based errors, configurable namespace/header/standard)
- [x] Dart target (`dart:ffi` bindings, enum generation, `pubspec.yaml`, null-safe code, configurable package name)
- [x] Go target (CGo bindings, Go `error` pattern, `go.mod`, idiomatic naming, configurable module path)
- [x] Ruby target (FFI gem bindings, struct class wrappers, gemspec, enum modules, configurable module/gem namespace)
- [x] Template engine (Tera) with user-overridable templates and template directory discovery
- [x] Pre-generation and post-generation hook commands
- [x] Inline `[generators.<target>]` sections in IDL files for per-target configuration
- [x] IR schema version field with documented compatibility guarantees
- [x] `weaveffi upgrade` command for migrating IDL files between schema versions
- [x] Shell auto-completions (`weaveffi completions <shell>` for bash, zsh, fish, PowerShell)
- [x] Improved `weaveffi new` with full project scaffold (Cargo.toml, lib.rs, IDL, README)
- [x] Typed handles (`handle<Name>`) replacing raw `u64` for type-safe handle usage
- [x] Benchmarking infrastructure (criterion) for codegen throughput
- [x] Async IR model and C ABI async convention (completion callbacks with context pointers)
- [x] Callback and listener patterns in the IR (register/unregister function pairs)
- [x] Iterator type in the IR for streaming/sequence patterns
- [x] Nested sub-module support in the IR
- [x] Builder pattern support for struct construction
- [x] Versioned API evolution: deprecated/since annotations, default field values
- [x] Borrowed types (`&str`, `&[u8]`) for zero-copy parameter passing
- [x] Mutable parameter annotations for safer codegen
- [x] Async-demo and events samples demonstrating callbacks, listeners, and iterators
- [x] Cross-module type references (struct in one module used as param in another)

## Future releases

### Quality and polish

- [ ] Align WASM generator with the C ABI error model (`out_err` parameter handling in generated JS)
- [ ] Complete Node N-API addon stub bodies (replace `// TODO: implement` with functional glue)
- [ ] Add end-to-end integration tests for JSON and TOML input formats
- [ ] Improve generator edge-case coverage (deeply nested optionals, maps of lists, enum-keyed maps)

### Safety and performance

- [ ] Zero-copy string and byte-buffer passing where safe (borrowed slices across the ABI boundary)
- [ ] Arena/pool allocation patterns for batch handle creation and destruction
- [ ] Profile and optimize IR parsing and validation for APIs with hundreds of functions
- [ ] Audit generated code for memory safety (double-free, use-after-free, null pointer paths)

### Async language mappings

- [ ] Swift `async/await` mapping for async functions
- [ ] Kotlin coroutine (`suspend fun`) mapping for async functions
- [ ] Node.js `Promise` mapping for async functions
- [ ] Python `asyncio` mapping for async functions
- [ ] .NET `Task<T>` / `async` mapping for async functions
- [ ] Cancellation token support for long-running async operations

### Dart Flutter integration

- [ ] Flutter plugin scaffold with platform channel integration

### Stability milestone

- [ ] Stability guarantees: IR schema, CLI interface, and Generator trait locked under SemVer
- [ ] Comprehensive migration guide for major version transitions
- [ ] Full cross-platform CI (add Windows to the test matrix)
- [ ] Security audit of all generated code patterns (memory safety, input validation)
- [ ] Published performance benchmarks for codegen throughput
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
