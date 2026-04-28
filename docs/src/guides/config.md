# Generator Configuration

## Overview

WeaveFFI ships with sensible defaults so `weaveffi generate api.yml`
just works. When you need to override package names, namespaces, or
the C ABI prefix, you have two options that compose with each other:

- A TOML file (`weaveffi.toml`) passed via `--config`. Per-environment
  values that vary by machine or CI runner.
- An inline `generators:` block inside the IDL. Project-wide values
  every contributor inherits without remembering a flag.

When the same option appears in both, the inline IDL value wins.

## When to use

- Use the **TOML config** when one developer or one pipeline needs to
  swap a value without changing the IDL.
- Use the **inline `generators:` block** when the value is part of the
  project contract (Swift module name, Go module path, custom
  C ABI prefix). Checking it into the IDL guarantees consistency.
- Use **both** when there is a project-wide default that an environment
  occasionally needs to override.

## Step-by-step

### 1. Pass a TOML config file

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
```

```toml
swift_module_name = "MyApp"
android_package = "com.example.myapp"
node_package_name = "@myorg/myapp"
wasm_module_name = "myapp_wasm"
c_prefix = "myapp"
strip_module_prefix = true
```

Every key is optional; omit anything you want defaulted.

### 2. Embed `generators:` in the IDL

```yaml
version: "0.3.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
generators:
  swift:
    module_name: MyAppFFI
  android:
    package: com.example.myapp
  c:
    prefix: myapp
  cpp:
    namespace: myapp
    header_name: myapp.hpp
    standard: "20"
  dart:
    package_name: my_dart_pkg
  go:
    module_path: github.com/example/myapp
  ruby:
    module_name: MyApp
    gem_name: myapp
  weaveffi:
    strip_module_prefix: true
    pre_generate: "cargo build --release"
```

Unknown target keys are silently ignored, so an older `weaveffi` CLI
can still read an IDL written for a newer one.

### 3. Verify the result

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
ls generated/
```

For day-to-day project recipes:

```toml
# iOS / macOS
swift_module_name = "MyAppFFI"
c_prefix = "myapp"
```

```toml
# Android
android_package = "com.example.myapp.ffi"
c_prefix = "myapp"
```

```toml
# Node
node_package_name = "@myorg/myapp-native"
```

### 4. Wire it into CI

`weaveffi diff --check` enforces that the committed bindings still
match the IDL. A typical guard job:

```yaml
# .github/workflows/ci.yml
- name: Verify generated bindings are up to date
  run: weaveffi diff api.yml --out generated --check
```

`weaveffi validate --format json` and `weaveffi lint --format json`
are designed to be parsed by quality dashboards:

```bash
weaveffi --quiet validate api.yml --format json | jq '.ok'
weaveffi --quiet lint api.yml --format json > lint-report.json || \
  (cat lint-report.json && exit 1)
```

## Reference

### TOML keys

| Key                    | Type   | Default            | Description                                                                 |
|------------------------|--------|--------------------|-----------------------------------------------------------------------------|
| `swift_module_name`    | string | `"WeaveFFI"`       | Swift module name in `Package.swift` and the `Sources/` directory           |
| `android_package`      | string | `"com.weaveffi"`   | Java/Kotlin package declaration in the JNI wrapper                          |
| `node_package_name`    | string | `"weaveffi"`       | npm package name in the Node.js loader                                      |
| `wasm_module_name`     | string | `"weaveffi_wasm"`  | Module name in the WASM JS loader                                           |
| `c_prefix`             | string | `"weaveffi"`       | Prefix prepended to every C ABI symbol (`{prefix}_{module}_{function}`)     |
| `strip_module_prefix`  | bool   | `false`            | Strip the module name from generated identifiers when supported             |
| `python_package_name`  | string | `"weaveffi"`       | Python package name                                                         |
| `dotnet_namespace`     | string | `"WeaveFFI"`       | .NET namespace                                                              |
| `cpp_namespace`        | string | `"weaveffi"`       | C++ namespace for the wrapper                                               |
| `cpp_header_name`      | string | `"weaveffi.hpp"`   | Header file name for the C++ output                                         |
| `cpp_standard`         | string | `"17"`             | C++ standard for the generated `CMakeLists.txt`                             |
| `dart_package_name`    | string | `"weaveffi"`       | Dart package name in `pubspec.yaml`                                         |
| `go_module_path`       | string | `"weaveffi"`       | Go module path in `go.mod`                                                  |
| `ruby_module_name`     | string | `"WeaveFFI"`       | Ruby module that wraps the bindings                                         |
| `ruby_gem_name`        | string | `"weaveffi"`       | Ruby gem name                                                               |
| `template_dir`         | string | _none_             | Directory of `.tera` overrides loaded by every generator                    |
| `pre_generate`         | string | _none_             | Shell command run before any generator starts                               |
| `post_generate`        | string | _none_             | Shell command run after every generator finishes                            |

### Inline `generators:` keys

Inline keys drop the `{target}_` prefix and live under their target's
subtable.

| Inline (IDL)                    | TOML field             | Type   |
|---------------------------------|------------------------|--------|
| `swift.module_name`             | `swift_module_name`    | string |
| `android.package`               | `android_package`      | string |
| `node.package_name`             | `node_package_name`    | string |
| `wasm.module_name`              | `wasm_module_name`     | string |
| `c.prefix`                      | `c_prefix`             | string |
| `python.package_name`           | `python_package_name`  | string |
| `dotnet.namespace`              | `dotnet_namespace`     | string |
| `cpp.namespace`                 | `cpp_namespace`        | string |
| `cpp.header_name`               | `cpp_header_name`      | string |
| `cpp.standard`                  | `cpp_standard`         | string |
| `dart.package_name`             | `dart_package_name`    | string |
| `go.module_path`                | `go_module_path`       | string |
| `ruby.module_name`              | `ruby_module_name`     | string |
| `ruby.gem_name`                 | `ruby_gem_name`        | string |
| `weaveffi.strip_module_prefix`  | `strip_module_prefix`  | bool   |
| `weaveffi.template_dir`         | `template_dir`         | string |
| `weaveffi.pre_generate`         | `pre_generate`         | string |
| `weaveffi.post_generate`        | `post_generate`        | string |

The alias `global` is accepted for the `weaveffi` section.

### Performance and CI flags

- The orchestrator dispatches every selected generator in parallel
  using [rayon](https://docs.rs/rayon). The pre- and post-generate
  hooks still run serially around the whole batch.
- Each generator persists a hash under
  `{out_dir}/.weaveffi-cache/{target}.hash`. Only generators whose
  hash changed are re-run; pass `--force` to invalidate every entry.
- `weaveffi diff --check` exit codes:

  | Code | Meaning |
  |------|---------|
  | `0`  | The committed output matches the IDL exactly. |
  | `2`  | One or more files would change in place. |
  | `3`  | One or more files would be added or removed. |

- `weaveffi validate --format json` emits structured success/failure:

  ```json
  { "ok": true, "modules": 2, "functions": 8, "structs": 3, "enums": 1 }
  ```

  ```json
  {
    "ok": false,
    "errors": [
      {
        "code": "DuplicateFunctionName",
        "module": "math",
        "function": "add",
        "message": "duplicate function name in module 'math': add",
        "suggestion": "function names must be unique within a module; rename the duplicate"
      }
    ]
  }
  ```

- `weaveffi lint --format json` returns the warning list with stable
  `code` / `location` / `message` fields:

  ```json
  {
    "ok": false,
    "warnings": [
      {
        "code": "DeepNesting",
        "location": "math::compute::matrix",
        "message": "deep type nesting at math::compute::matrix (depth 4, max recommended 3)"
      }
    ]
  }
  ```

## Pitfalls

- **Inline value overrides TOML silently** — there is no warning when
  both are set. If a TOML override "doesn't take", check for an inline
  block in the IDL.
- **`c_prefix` rewrites every generator** — picking a custom prefix
  also rewrites the runtime symbols (`{prefix}_free_string`, ...). The
  Rust cdylib must be built with the same prefix.
- **`strip_module_prefix = true` flattens names** — collisions across
  modules become possible. Pick one or the other consistently.
- **Hooks run shell commands as-is** — `pre_generate` and
  `post_generate` are passed straight to `sh -c`. Quote them
  carefully and never include untrusted input.
- **Cache only tracks IR + generator name** — if you change a template
  override outside the IR (e.g. via `template_dir`) and want all
  generators to re-run, pass `--force`.
- **Older CLIs ignore unknown keys** — adding a new generator key
  with a project-wide implication does not error out on older
  toolchains. Pin the CLI version in CI when you need that guarantee.
