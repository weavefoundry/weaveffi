# Getting Started

## Install WeaveFFI

Install the CLI from source via Cargo:

```bash
cargo install weaveffi-cli
```

This installs the `weaveffi` binary on your `PATH`.

## Create a new project

Scaffold a starter project with an example IR file:

```bash
weaveffi new my-library
cd my-library
```

## Define your IR

Edit the generated YAML file to describe your functions:

```yaml
version: "0.1.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
```

## Generate bindings

```bash
weaveffi generate my-library.yml -o generated
```

This produces target-specific output under `generated/`:

- `c/` — C header and convenience stubs
- `swift/` — SwiftPM System Library (`CWeaveFFI`) and Swift wrapper (`WeaveFFI`)
- `android/` — Kotlin JNI wrapper + Gradle skeleton
- `node/` — N-API addon loader + TypeScript type definitions
- `wasm/` — minimal WASM loader stub

## Check your toolchain

Run the doctor command to verify prerequisites:

```bash
weaveffi doctor
```

This checks for Rust, Xcode (macOS), Android NDK, and Node.js toolchains
and reports actionable guidance for anything missing.

## Local docs preview

If you want to preview or contribute to the documentation:

```bash
cargo install mdbook
mdbook serve docs -p 3000 -n 127.0.0.1
```

Open <http://127.0.0.1:3000>.
