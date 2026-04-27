# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli)

WeaveFFI is a CLI code-generation tool that takes an API definition written in
YAML, JSON, or TOML and produces idiomatic bindings for C, C++, Swift, Android
(Kotlin/JNI), Node.js (N-API), WebAssembly, Python (ctypes), .NET (P/Invoke),
Dart (dart:ffi), Go (CGo), and Ruby (FFI gem) — all through a stable C ABI. No
hand-written JNI glue, no duplicate implementations; one definition, every
platform.

WeaveFFI works with any native library that can expose a stable C ABI — whether
it's written in Rust, C, C++, Zig, or another language. Rust has first-class
scaffolding support today via `--scaffold`; other languages implement the
functions declared in the generated C header directly.

Generated packages are designed to be standalone and publishable — consumers
install a normal ecosystem package (npm, SwiftPM, Gradle, pub, gem) without
needing WeaveFFI tooling or runtime dependencies.

## Features

- **Multi-format input** — define your API in YAML, JSON, or TOML
- **Rich type system**
  - Primitives: `i32`, `u32`, `i64`, `f64`, `bool`
  - `string` (UTF-8), `bytes`, `handle` (opaque pointer)
  - Typed handles: `handle<T>` for compile-time safe opaque pointers
  - Borrowed types: `&str`, `&[u8]` for zero-copy parameter passing
  - Iterators: `iter<T>` for lazy streaming sequences
  - User-defined structs with typed fields
  - Enums with explicit integer discriminants
  - Optionals (`string?`, `Contact?`)
  - Lists (`[i32]`, `[Contact]`)
  - Maps (`{string:i32}`, `{string:Contact}`)
  - Callbacks and event listeners
- **Eleven target languages** from one definition (see table below)
- **Async support** — functions marked `async: true` generate callback-based C ABI signatures with idiomatic wrappers (Swift `async/await`, Kotlin `suspend`, Python `async def`, Node `Promise`, .NET `Task<T>`, C++ `std::future`)
- **Builder pattern** — structs with `builder: true` generate fluent builder classes in every target language
- **Cross-module types** — structs and enums defined in one module can be referenced from another
- **Nested modules** — modules can contain sub-modules for hierarchical API organization
- **Template engine** — override built-in code generation with custom [Tera](https://keats.github.io/tera/) templates via `--templates`
- **Hook commands** — run arbitrary shell commands before and after generation via `pre_generate` / `post_generate` in the config
- **Inline generator config** — embed per-target configuration directly in your IDL file via a `generators` section
- **Validation** — catches duplicate names, unknown type references, reserved keywords, and invalid identifiers before code generation
- **Extract** — `weaveffi extract` reads annotated Rust source files and produces an API definition, so you don't have to write IDL by hand
- **Scaffolding** — `--scaffold` flag emits a Rust `extern "C"` stub file so you can fill in the implementation
- **Generator configuration** — customise Swift module names, Android package, C prefix, C++ namespace, Dart/Go/Ruby package names, and more via a TOML config file (see [docs](https://weavefoundry.github.io/weaveffi/guides/config.html))
- **Schema versioning** — IR version field with `weaveffi schema-version` for querying the current version
- **Doctor** — `weaveffi doctor` checks for required toolchains (Rust, Xcode, NDK, Node)

## Supported targets

| Target | Output directory | What you get |
|--------|-----------------|--------------|
| **C** | `c/` | `weaveffi.h` header with struct typedefs, function prototypes, and error types |
| **C++** | `cpp/` | RAII header (`weaveffi.hpp`) with move semantics, `std::optional`/`std::vector`/`std::unordered_map` wrappers, exception-based errors, and CMakeLists.txt |
| **Swift** | `swift/` | SwiftPM package with a thin Swift wrapper over the C ABI |
| **Android** | `android/` | Kotlin JNI wrapper + Gradle project skeleton |
| **Node.js** | `node/` | N-API addon loader + TypeScript type definitions |
| **WASM** | `wasm/` | JavaScript loader stub + TypeScript declarations for `wasm32-unknown-unknown` builds |
| **Python** | `python/` | ctypes bindings + `.pyi` type stubs + pip-installable package |
| **.NET** | `dotnet/` | C# P/Invoke bindings + `.csproj` + `.nuspec` for NuGet |
| **Dart** | `dart/` | `dart:ffi` bindings + `pubspec.yaml` for Flutter/Dart projects |
| **Go** | `go/` | CGo bindings + `go.mod` for Go modules |
| **Ruby** | `ruby/` | FFI gem bindings + `.gemspec` for RubyGems |

## Quickstart

1. **Install the CLI**

```bash
cargo install weaveffi-cli
```

2. **Define your API** in a YAML file (e.g. `contacts.yml`):

```yaml
version: "0.3.0"
modules:
  - name: contacts
    structs:
      - name: Contact
        fields:
          - name: id
            type: i64
          - name: name
            type: string
          - name: email
            type: "string?"
    functions:
      - name: create_contact
        params:
          - name: name
            type: string
          - name: email
            type: "string?"
        return: Contact
      - name: list_contacts
        params: []
        return: "[Contact]"
```

3. **Generate bindings**

```bash
weaveffi generate contacts.yml -o generated --target c,swift,python
```

4. **Inspect the output** — for example, the generated C header (`generated/c/weaveffi.h`) contains:

```c
typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;

weaveffi_contacts_Contact* weaveffi_contacts_Contact_create(
    int64_t id,
    const uint8_t* name_ptr, size_t name_len,
    const uint8_t* email_ptr, size_t email_len,
    weaveffi_error* out_err);

void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);

const char* weaveffi_contacts_Contact_get_name(
    const weaveffi_contacts_Contact* ptr);

weaveffi_contacts_Contact* weaveffi_contacts_create_contact(
    const uint8_t* name_ptr, size_t name_len,
    const uint8_t* email_ptr, size_t email_len,
    weaveffi_error* out_err);
```

## CLI commands

| Command | Description |
|---------|-------------|
| `weaveffi generate <file> -o <dir>` | Generate bindings for all targets |
| `weaveffi generate <file> -o <dir> --target c,swift,cpp` | Generate only specific targets |
| `weaveffi generate <file> -o <dir> --scaffold` | Also emit a Rust FFI stub file |
| `weaveffi generate <file> -o <dir> --config cfg.toml` | Apply generator configuration |
| `weaveffi generate <file> -o <dir> --templates tpl/` | Use custom Tera templates for code generation |
| `weaveffi validate <file>` | Validate an API definition without generating |
| `weaveffi extract <file.rs>` | Extract an API definition from annotated Rust source |
| `weaveffi new <name>` | Scaffold a new project with a starter API definition and Cargo.toml |
| `weaveffi lint <file>` | Lint an API definition and report warnings |
| `weaveffi diff <file>` | Show a diff of what would change if bindings were regenerated |
| `weaveffi doctor` | Check for required toolchains (Rust, Xcode, NDK, Node) |
| `weaveffi completions <shell>` | Print shell completion scripts (bash, zsh, fish, etc.) |
| `weaveffi schema-version` | Print the current IR schema version |

## Documentation

Full docs are available at the [WeaveFFI documentation site](https://weavefoundry.github.io/weaveffi/).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
