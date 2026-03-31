# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli)

WeaveFFI is a CLI code-generation tool that takes an API definition written in
YAML, JSON, or TOML and produces idiomatic bindings for C, Swift, Android
(Kotlin/JNI), Node.js (N-API), WebAssembly, Python (ctypes), and .NET
(P/Invoke) — all through a stable C ABI. No hand-written JNI glue, no
duplicate implementations; one definition, every platform.

WeaveFFI works with any native library that can expose a stable C ABI — whether
it's written in Rust, C, C++, Zig, or another language. Rust has first-class
scaffolding support today via `--scaffold`; other languages implement the
functions declared in the generated C header directly.

Generated packages are designed to be standalone and publishable — consumers
install a normal ecosystem package (npm, SwiftPM, Gradle) without needing
WeaveFFI tooling or runtime dependencies.

## Features

- **Multi-format input** — define your API in YAML, JSON, or TOML
- **Rich type system**
  - Primitives: `i32`, `u32`, `i64`, `f64`, `bool`
  - `string` (UTF-8), `bytes`, `handle` (opaque pointer)
  - User-defined structs with typed fields
  - Enums with explicit integer discriminants
  - Optionals (`string?`, `Contact?`)
  - Lists (`[i32]`, `[Contact]`)
  - Maps (`{string:i32}`, `{string:Contact}`)
- **Seven target languages** from one definition (see table below)
- **Validation** — catches duplicate names, unknown type references, reserved keywords, and invalid identifiers before code generation
- **Extract** — `weaveffi extract` reads annotated Rust source files and produces an API definition, so you don't have to write IDL by hand
- **Scaffolding** — `--scaffold` flag emits a Rust `extern "C"` stub file so you can fill in the implementation
- **Generator configuration** — customise Swift module names, Android package, Node package name, and more via a TOML config file (see [docs](https://weavefoundry.github.io/weaveffi/guides/config.html))
- **Doctor** — `weaveffi doctor` checks for required toolchains (Rust, Xcode, NDK, Node)

## Supported targets

| Target | Output directory | What you get |
|--------|-----------------|--------------|
| **C** | `c/` | `weaveffi.h` header with struct typedefs, function prototypes, and error types |
| **Swift** | `swift/` | SwiftPM package with a thin Swift wrapper over the C ABI |
| **Android** | `android/` | Kotlin JNI wrapper + Gradle project skeleton |
| **Node.js** | `node/` | N-API addon loader + TypeScript type definitions |
| **WASM** | `wasm/` | JavaScript loader stub for `wasm32-unknown-unknown` builds |
| **Python** | `python/` | ctypes bindings + `.pyi` type stubs + pip-installable package |
| **.NET** | `dotnet/` | C# P/Invoke bindings + `.csproj` + `.nuspec` for NuGet |

## Quickstart

1. **Install the CLI**

```bash
cargo install weaveffi-cli
```

2. **Define your API** in a YAML file (e.g. `contacts.yml`):

```yaml
version: "0.1.0"
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
| `weaveffi generate <file> -o <dir> --target c,swift` | Generate only specific targets |
| `weaveffi generate <file> -o <dir> --scaffold` | Also emit a Rust FFI stub file |
| `weaveffi generate <file> -o <dir> --config cfg.toml` | Apply generator configuration |
| `weaveffi validate <file>` | Validate an API definition without generating |
| `weaveffi extract <file.rs>` | Extract an API definition from annotated Rust source |
| `weaveffi new <name>` | Scaffold a new project with a starter API definition |
| `weaveffi doctor` | Check for required toolchains (Rust, Xcode, NDK, Node) |

## Documentation

Full docs are available at the [WeaveFFI documentation site](https://weavefoundry.github.io/weaveffi/).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
