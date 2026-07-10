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
[swift]
module_name = "MyApp"

[android]
package = "com.example.myapp"

[node]
package_name = "@myorg/myapp"

[wasm]
module_name = "myapp_wasm"

[c]
prefix = "myapp"

[global]
strip_module_prefix = false
```

Every section and key is optional; omit anything you want defaulted.
The `[global]` table accepts the alias `[weaveffi]`. Module-prefix
stripping is on by default, so the useful direction for
`strip_module_prefix` is `false`: one `[global]` line restores
module-prefixed wrapper names across every supporting target.

### 2. Embed `generators:` in the IDL

```yaml
version: "0.5.0"
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
    strip_module_prefix: false
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
[swift]
module_name = "MyAppFFI"

[c]
prefix = "myapp"
```

```toml
# Android
[android]
package = "com.example.myapp.ffi"

[c]
prefix = "myapp"
```

```toml
# Node
[node]
package_name = "@myorg/myapp-native"
```

The C ABI symbol prefix is global by nature: every consumer must call
the identical exported symbols. The CLI resolves it once
(`[global] c_prefix` wins, then `[c] prefix`) and fans it out to every
per-target config that hasn't set its own `prefix`, so a custom prefix
is honored across all eleven languages, not just C and C++.

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

TOML config files and inline IDL `generators:` blocks share the same
section names and key names. Pick the location that fits your workflow;
the keys are identical.

### Per-target sections

| Section     | Key                    | Type   | Default            | Description                                                                 |
|-------------|------------------------|--------|--------------------|-----------------------------------------------------------------------------|
| `[swift]`   | `module_name`          | string | `"WeaveFFI"`       | Swift module name in `Package.swift` and the `Sources/` directory           |
| `[swift]`   | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Swift symbols                       |
| `[android]` | `package`              | string | `"com.weaveffi"`   | Java/Kotlin package declaration in the JNI wrapper                          |
| `[android]` | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Java/Kotlin symbols                 |
| `[node]`    | `package_name`         | string | `"weaveffi"`       | npm package name in the Node.js loader                                      |
| `[node]`    | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted JS/TS symbols                       |
| `[wasm]`    | `module_name`          | string | `"weaveffi_wasm"`  | Module name in the Wasm JS loader                                           |
| `[wasm]`    | `allow_unsupported`    | bool   | `false`            | Generate anyway when the IDL uses features Wasm cannot deliver (callbacks, listeners); unsupported entry points become explicit throwing stubs |
| `[wasm]`    | `emscripten`           | bool   | `false`            | Target an Emscripten build: the loader accepts a pre-initialized Emscripten `Module` (or its `MODULARIZE` factory promise) instead of a `.wasm` URL; async functions become throwing stubs |
| `[c]`       | `prefix`               | string | `"weaveffi"`       | Prefix prepended to every C ABI symbol (`{prefix}_{module}_{function}`)     |
| `[cpp]`     | `namespace`            | string | `"weaveffi"`       | C++ namespace for the wrapper                                               |
| `[cpp]`     | `header_name`          | string | `"weaveffi.hpp"`   | Header file name for the C++ output                                         |
| `[cpp]`     | `standard`             | string | `"17"`             | C++ standard for the generated `CMakeLists.txt`                             |
| `[python]`  | `package_name`         | string | `"weaveffi"`       | Python package name                                                         |
| `[python]`  | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Python symbols                      |
| `[dotnet]`  | `namespace`            | string | `"WeaveFFI"`       | .NET namespace                                                              |
| `[dotnet]`  | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted C# symbols                          |
| `[dart]`    | `package_name`         | string | `"weaveffi"`       | Dart package name in `pubspec.yaml`                                         |
| `[dart]`    | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Dart symbols                        |
| `[go]`      | `module_path`          | string | `"weaveffi"`       | Go module path in `go.mod`                                                  |
| `[go]`      | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Go symbols                          |
| `[ruby]`    | `module_name`          | string | `"WeaveFFI"`       | Ruby module that wraps the bindings                                         |
| `[ruby]`    | `gem_name`             | string | `"weaveffi"`       | Ruby gem name                                                               |
| `[ruby]`    | `strip_module_prefix`  | bool   | `true`             | Strip the IR module prefix from emitted Ruby symbols                        |

Every per-target section also accepts a `prefix` key naming the C ABI
symbol prefix its wrappers call. You rarely set it per target: the CLI
fans the resolved global prefix (`[global] c_prefix`, or `[c] prefix`)
out to every section that leaves it unset, so all eleven targets call
the same exported symbols.

> **Package identity.** The name, version, and metadata stamped into every
> generated manifest are resolved from the IDL
> [`package:` block](../reference/idl.md#package-metadata) by one shared
> policy. For an identity value an explicit key below wins; otherwise it falls
> back to the `package:` name (normalized per ecosystem), then the IDL file
> stem, then the `"weaveffi"`/`"WeaveFFI"` default shown above. The keys that
> participate are `[swift] module_name`, `[node] package_name`,
> `[python] package_name`, `[dart] package_name`, `[go] module_path`,
> `[ruby] gem_name`, and `[dotnet] namespace` (which also sets the NuGet
> package id). Manifests with no dedicated key (Android `rootProject.name`,
> the Wasm `package.json`, and the C++ `CMakeLists.txt` version) follow the
> same identity, and the published version comes from `package.version`
> (default `0.1.0`). All other keys (e.g. `[c] prefix`, `[cpp] namespace`,
> `[android] package`, `[ruby] module_name`, `[wasm] module_name`) keep the
> fixed defaults above.

### `[global]` section

| Key                    | Type   | Default            | Description                                                                 |
|------------------------|--------|--------------------|-----------------------------------------------------------------------------|
| `strip_module_prefix`  | bool   | _unset_            | Shorthand: sets `strip_module_prefix` on every target that supports it, overriding their sections. Stripping is on by default, so `false` restores module-prefixed names everywhere at once |
| `c_prefix`             | string | _unset_            | Global C ABI symbol prefix, fanned out to every per-target `prefix` that is unset; wins over `[c] prefix` as the resolution source |
| `pre_generate`         | string | _none_             | Shell command run before any generator starts                               |
| `post_generate`        | string | _none_             | Shell command run after every generator finishes                            |

The alias `[weaveffi]` is accepted for the `[global]` section.

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

- **Inline value overrides TOML silently**: there is no warning when
  both are set. If a TOML override "doesn't take", check for an inline
  block in the IDL.
- **The C prefix rewrites every generator**: picking a custom prefix
  also rewrites the runtime symbols (`{prefix}_free_string`, ...). The
  Rust cdylib must be built with the same prefix. Every wrapper picks
  it up automatically from the resolved global value; if you also set
  a per-target `prefix`, make sure they agree.
- **Module-prefix stripping flattens names**: it's on by default, so
  two modules that each declare an `open` function collide in targets
  with a flat namespace. Rename one, or set
  `strip_module_prefix = false` (globally or per target) to restore
  prefixed names.
- **Hooks run shell commands as-is**: `pre_generate` and
  `post_generate` are passed straight to `sh -c`. Quote them
  carefully and never include untrusted input.
- **Cache covers IR, generator name, generator config, and CLI version**:
  changing the IR, any generator config field, or upgrading the CLI
  invalidates the per-generator cache and triggers re-emission.
- **Older CLIs ignore unknown keys**: adding a new generator key
  with a project-wide implication does not error out on older
  toolchains. Pin the CLI version in CI when you need that guarantee.
