# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT) [![crates.io](https://img.shields.io/crates/v/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli) [![Schema](https://img.shields.io/badge/schema-0.3.0-orange)](./weaveffi.schema.json) [![downloads](https://img.shields.io/crates/d/weaveffi-cli.svg)](https://crates.io/crates/weaveffi-cli)

WeaveFFI generates type-safe bindings for 11 languages from a single IDL —
no hand-written JNI, no duplicate implementations, no unsafe boilerplate.
Define your API once in YAML, JSON, or TOML; ship idiomatic packages for
C, C++, Swift, Kotlin/Android, Node.js, WebAssembly, Python, .NET, Dart,
Go, and Ruby that all talk to the same stable C ABI.

## Quickstart

**1. Install the CLI:**

```bash
cargo install weaveffi-cli
```

**2. Define your API** in `contacts.yml`:

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

**3. Generate bindings:**

```bash
weaveffi generate contacts.yml -o generated --target c,swift,python,node,dart
```

**4. Use the generated code from any of the eleven supported languages.**
Click each block below to see what WeaveFFI emits.

<details>
<summary><strong>C</strong> — <code>generated/c/weaveffi.h</code></summary>

```c
typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;

weaveffi_contacts_Contact* weaveffi_contacts_Contact_create(
    int64_t id,
    const char* name,
    const char* email,
    weaveffi_error* out_err);
void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);

const char* weaveffi_contacts_Contact_get_name(
    const weaveffi_contacts_Contact* ptr);

weaveffi_contacts_Contact* weaveffi_contacts_create_contact(
    const char* name,
    const char* email,
    weaveffi_error* out_err);

weaveffi_contacts_Contact** weaveffi_contacts_list_contacts(
    size_t* out_len, weaveffi_error* out_err);
```

</details>

<details>
<summary><strong>Swift</strong> — <code>generated/swift/Sources/WeaveFFI/WeaveFFI.swift</code></summary>

```swift
public class Contact {
    let ptr: OpaquePointer

    init(ptr: OpaquePointer) {
        self.ptr = ptr
    }

    deinit {
        weaveffi_contacts_Contact_destroy(ptr)
    }

    public var id: Int64 {
        return weaveffi_contacts_Contact_get_id(ptr)
    }

    public var name: String {
        let raw = weaveffi_contacts_Contact_get_name(ptr)
        guard let raw = raw else { return "" }
        defer { weaveffi_free_string(raw) }
        return String(cString: raw)
    }

    public var email: String? { /* ... */ }
}

public enum Contacts {
    public static func contacts_create_contact(_ name: String, _ email: String?) throws -> Contact { /* ... */ }
    public static func contacts_list_contacts() throws -> [Contact] { /* ... */ }
}
```

</details>

<details>
<summary><strong>Python</strong> — <code>generated/python/weaveffi/weaveffi.pyi</code></summary>

```python
from typing import List, Optional

class Contact:
    @property
    def id(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def email(self) -> Optional[str]: ...

def contacts_create_contact(name: str, email: Optional[str]) -> "Contact": ...
def contacts_list_contacts() -> List["Contact"]: ...
```

</details>

<details>
<summary><strong>TypeScript</strong> — <code>generated/node/types.d.ts</code></summary>

```typescript
export interface Contact {
  id: number;
  name: string;
  email: string | null;
}

export function contacts_create_contact(
  name: string,
  email: string | null,
): Contact;

export function contacts_list_contacts(): Contact[];
```

</details>

<details>
<summary><strong>Dart</strong> — <code>generated/dart/lib/weaveffi.dart</code></summary>

```dart
class Contact {
  final Pointer<Void> _handle;
  Contact._(this._handle);

  void dispose() { /* destroy native handle */ }

  int get id { /* ... */ }
  String get name { /* ... */ }
  String? get email { /* ... */ }
}

Contact createContact(String name, String? email) { /* ... */ }
List<Contact> listContacts() { /* ... */ }
```

</details>

## Why WeaveFFI?

- **One IDL, eleven languages.** Describe your API once and ship packages to
  npm, SwiftPM, Maven, PyPI, NuGet, pub.dev, RubyGems, and Go modules. Each
  package is standalone — consumers don't need WeaveFFI installed.
- **Stable C ABI underneath.** Every target speaks to the same `extern "C"`
  contract, so adding a new platform later is a code-gen change, not a
  rewrite. Works with any backend that can expose a C ABI: Rust (with
  first-class scaffolding via `--scaffold`), C, C++, or Zig.
- **Idiomatic per-target output.** No lowest-common-denominator surface area.
  Swift gets `async/await` and `throws`, Kotlin gets `suspend` and JNI glue,
  Python gets typed `.pyi` stubs, TypeScript gets `Promise`s, Dart gets
  `dart:ffi` — all from the same definition.

## How does it compare?

See [Comparison](docs/src/comparison.md) for a side-by-side feature matrix
versus UniFFI, cbindgen, diplomat, SWIG, and autocxx, plus an honest
"when to choose WeaveFFI" guide.

## Supported targets

| Target | Output directory | What you get |
|--------|------------------|--------------|
| **C** | `c/` | `weaveffi.h` header with struct typedefs, function prototypes, and the shared `weaveffi_error` type |
| **C++** | `cpp/` | RAII header (`weaveffi.hpp`) with move semantics, `std::optional`/`std::vector`/`std::unordered_map` wrappers, exception-based errors, and a `CMakeLists.txt` |
| **Swift** | `swift/` | SwiftPM package wrapping the C ABI with `throws`, `async/await`, and `Codable`-friendly types |
| **Android** | `android/` | Kotlin JNI wrapper, C shim, and a Gradle project skeleton |
| **Node.js** | `node/` | N-API addon loader + TypeScript declarations and a `package.json` |
| **WASM** | `wasm/` | JavaScript loader + TypeScript declarations for `wasm32-unknown-unknown` builds |
| **Python** | `python/` | `ctypes` bindings + `.pyi` type stubs + `pyproject.toml` |
| **.NET** | `dotnet/` | C# P/Invoke bindings + `.csproj` + `.nuspec` for NuGet |
| **Dart** | `dart/` | `dart:ffi` bindings + `pubspec.yaml` for Flutter and Dart projects |
| **Go** | `go/` | CGo bindings + `go.mod` for Go modules |
| **Ruby** | `ruby/` | FFI gem bindings + `.gemspec` for RubyGems |

## Install

**From crates.io** (requires the [Rust toolchain](https://rustup.rs/)):

```bash
cargo install weaveffi-cli
```

**Pre-built binaries** for macOS, Linux, and Windows are attached to every
[GitHub release](https://github.com/weavefoundry/weaveffi/releases) — download
the archive for your platform, extract the `weaveffi` binary, and put it on
your `PATH`.

Verify the install:

```bash
weaveffi --version
weaveffi schema-version    # prints 0.3.0
```

## CLI reference

| Command | Description |
|---------|-------------|
| `weaveffi new <name>` | Scaffold a new project with a starter IDL and `Cargo.toml` |
| `weaveffi generate <file> -o <dir>` | Generate bindings; `--target c,swift,...` to subset, `--scaffold` to emit Rust FFI stubs, `--config cfg.toml` for generator options, `--templates dir/` for custom Tera overrides, `--dry-run` to preview |
| `weaveffi validate <file>` | Validate an IDL definition without generating; `--format json` for machine-readable output |
| `weaveffi lint <file>` | Lint an IDL and report non-fatal warnings |
| `weaveffi diff <file>` | Show what would change if bindings were regenerated; `--check` for CI |
| `weaveffi extract <file.rs>` | Extract an IDL from annotated Rust source (alternative to writing IDL by hand) |
| `weaveffi format <file>` | Rewrite an IDL file in canonical form (sorted keys); `--check` for CI |
| `weaveffi watch <file>` | Re-run `generate` whenever the IDL file changes |
| `weaveffi upgrade <file>` | Migrate an older IDL to the current schema version; `--check` for CI |
| `weaveffi schema --format json-schema` | Print the JSON Schema for the IDL |
| `weaveffi schema-version` | Print the current IR schema version (`0.3.0`) |
| `weaveffi doctor` | Check for required toolchains; `--target swift` to scope to one language, `--format json` for CI |
| `weaveffi completions <shell>` | Print shell completion scripts (`bash`, `zsh`, `fish`, `powershell`, `elvish`) |

Reference the JSON Schema from your IDL for editor autocompletion:

```yaml
# yaml-language-server: $schema=./weaveffi.schema.json
version: "0.3.0"
modules: ...
```

Regenerate the schema with `weaveffi schema --format json-schema > weaveffi.schema.json`.

## Documentation

Full documentation lives at <https://docs.weaveffi.com/> (sources under
[`docs/`](./docs/)). Key pages:

- [Introduction](docs/src/intro.md) — what WeaveFFI is and why it exists
- [Getting Started](docs/src/getting-started.md) — install → IDL → generate → call from C
- [Comparison](docs/src/comparison.md) — feature matrix vs UniFFI, cbindgen, diplomat, SWIG, autocxx
- [FAQ](docs/src/faq.md) — top questions about scope, runtime cost, and platform support
- [Generators](docs/src/generators/) — per-target reference for each of the eleven languages
- [Guides](docs/src/guides/) — memory ownership, error handling, async, configuration

## Status

WeaveFFI is in **pre-1.0**; expect breaking changes until **1.0.0**. The C ABI
naming convention (`{c_prefix}_{module}_{function}`), the `weaveffi-abi`
runtime symbols (`weaveffi_free_string`, `weaveffi_free_bytes`,
`weaveffi_error_clear`), and the IDL schema may all evolve in minor releases.
We follow the rule that any breaking change is gated behind a `weaveffi
upgrade` migration step so you can move IDLs forward mechanically.

Releases are fully automated by [semantic-release](https://semantic-release.gitbook.io/)
on merge to `main`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow, snapshot
test conventions, fuzzing setup, and Conventional Commit rules.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
