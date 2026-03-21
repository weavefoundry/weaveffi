# WeaveFFI

[![CI](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml/badge.svg)](https://github.com/weavefoundry/weaveffi/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)

WeaveFFI is a code generator that produces multi-language bindings from a
Rust library via a stable C ABI. Define your functions in a YAML file, run
`weaveffi generate`, and get idiomatic wrappers for C, Swift, Android
(Kotlin/JNI), Node.js (N-API), and WebAssembly.

Each target calls the same Rust core through a C ABI layer — no separate
re-implementations or hand-written JNI/bridging code.

## Quickstart

1. **Install the CLI**

```bash
cargo install weaveffi-cli
```

2. **Define your IR** in a YAML file (e.g. `calculator.yml`):

```yaml
version: "0.1.0"
modules:
  - name: calculator
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
```

3. **Generate bindings**

```bash
weaveffi generate calculator.yml -o generated
```

4. **Use the output** — the `generated/` directory contains:
   - `c/` — C header with function prototypes and error types
   - `swift/` — SwiftPM System Library + thin Swift wrapper
   - `android/` — Kotlin JNI wrapper + Gradle skeleton
   - `node/` — N-API addon loader + TypeScript type definitions
   - `wasm/` — minimal WASM loader stub

## CLI commands

| Command | Description |
|---|---|
| `weaveffi generate <ir> -o <dir>` | Generate bindings from an IR file |
| `weaveffi new <name>` | Scaffold a new project with a starter IR |
| `weaveffi doctor` | Check for required toolchains (Rust, Xcode, NDK, Node) |

## Documentation

Full docs are available at the [WeaveFFI documentation site](https://weavefoundry.github.io/weaveffi/).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
