# Generator Configuration

WeaveFFI reads an optional TOML configuration file that lets you customise
names, packages, and prefixes used by each code generator. When no configuration
file is provided, every option falls back to a sensible default.

## Passing the configuration file

Use the `--config` flag on the `generate` command:

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
```

When `--config` is omitted, all options use their default values.

## File format

The configuration file is plain TOML. All keys are top-level — there are no
nested tables. Every key is optional; omit a key to keep its default.

### Minimal example

An empty file (or no file at all) is valid — defaults apply to everything:

```toml
# weaveffi.toml — all defaults
```

### Full example

```toml
# weaveffi.toml

# Swift module name used in Package.swift and the Sources/ directory.
swift_module_name = "MyApp"

# Java/Kotlin package name for the Android JNI wrapper.
android_package = "com.example.myapp"

# npm package name emitted in the Node.js loader.
node_package_name = "@myorg/myapp"

# WASM module name used in the JavaScript loader.
wasm_module_name = "myapp_wasm"

# Prefix for C ABI symbol names (e.g. myapp_math_add instead of weaveffi_math_add).
c_prefix = "myapp"

# When true, strip the module name from generated identifiers where applicable.
strip_module_prefix = true
```

## Configuration options

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `swift_module_name` | string | `"WeaveFFI"` | Name of the Swift module in `Package.swift` and the `Sources/` directory. |
| `android_package` | string | `"com.weaveffi"` | Java/Kotlin package declaration in the generated JNI wrapper. |
| `node_package_name` | string | `"weaveffi"` | Package name in the generated Node.js N-API loader. |
| `wasm_module_name` | string | `"weaveffi_wasm"` | Module name in the generated WASM JavaScript loader. |
| `c_prefix` | string | `"weaveffi"` | Prefix prepended to every C ABI symbol (`{prefix}_{module}_{function}`). |
| `strip_module_prefix` | bool | `false` | Strip the module name from generated identifiers where applicable. |

### `swift_module_name`

Controls the Swift package and module name. The generated `Package.swift`
references this name, and the source directory is created as
`Sources/{swift_module_name}/`.

```toml
swift_module_name = "CoolLib"
```

Produces `Sources/CoolLib/CoolLib.swift` and a matching `Package.swift`:

```swift
// Package.swift
let package = Package(
    name: "CoolLib",
    ...
    targets: [
        .target(name: "CoolLib", path: "Sources/CoolLib"),
    ]
)
```

### `android_package`

Sets the Java/Kotlin package for the generated JNI bridge. The package
determines the directory structure and the `package` declaration at the top of
the generated `.kt` file.

```toml
android_package = "com.example.myapp"
```

Produces:

```kotlin
package com.example.myapp
```

### `node_package_name`

Sets the package name used in the generated Node.js loader. This is the name
consumers use in `require()` or `import` statements.

```toml
node_package_name = "@myorg/cool-lib"
```

### `wasm_module_name`

Sets the module name used in the generated WASM JavaScript loader and
TypeScript declarations.

```toml
wasm_module_name = "coolapp_wasm"
```

### `c_prefix`

Replaces the default `weaveffi` prefix on all C ABI symbol names. This affects
every generated header and every language binding that references C symbols.

```toml
c_prefix = "myapp"
```

With an API module named `math` containing a function `add`, the exported C
symbol becomes `myapp_math_add` instead of the default `weaveffi_math_add`.

### `strip_module_prefix`

When set to `true`, generated identifiers omit the module name where the
target language supports namespacing natively.

```toml
strip_module_prefix = true
```

## Common recipes

### iOS/macOS project

```toml
swift_module_name = "MyAppFFI"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated -t swift,c --config weaveffi.toml
```

### Android project

```toml
android_package = "com.example.myapp.ffi"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated -t android --config weaveffi.toml
```

### Node.js package

```toml
node_package_name = "@myorg/myapp-native"
```

```bash
weaveffi generate api.yml -o generated -t node --config weaveffi.toml
```

### All targets with custom prefix

```toml
swift_module_name = "MyAppFFI"
android_package = "com.example.myapp"
node_package_name = "@example/myapp"
wasm_module_name = "myapp_wasm"
c_prefix = "myapp"
```

```bash
weaveffi generate api.yml -o generated --config weaveffi.toml
```

## Inline generator configuration

Every key from the TOML config file can also live directly inside the IDL
under a top-level `generators:` map. Each subtable is a *target name* (or
the special `weaveffi` / `global` section); inside each target, the keys
omit the `{target}_` prefix from the TOML form.

When the same option appears in both places, the inline IDL value wins.
The IDL is checked into the repository alongside the API definition and
is therefore the more specific, project-local source of truth, while a
`--config` TOML file is typically per-environment (CI, developer machine,
release pipeline). Putting an option in the IDL guarantees every
consumer sees the same generator settings without remembering to pass
`--config`.

### Per-target keys

Each entry below shows the inline form on the left and the equivalent
TOML field on the right.

| Inline (IDL) | TOML field | Type |
|--------------|------------|------|
| `swift.module_name` | `swift_module_name` | string |
| `android.package` | `android_package` | string |
| `node.package_name` | `node_package_name` | string |
| `wasm.module_name` | `wasm_module_name` | string |
| `c.prefix` | `c_prefix` | string |
| `python.package_name` | `python_package_name` | string |
| `dotnet.namespace` | `dotnet_namespace` | string |
| `cpp.namespace` | `cpp_namespace` | string |
| `cpp.header_name` | `cpp_header_name` | string |
| `cpp.standard` | `cpp_standard` | string |
| `dart.package_name` | `dart_package_name` | string |
| `go.module_path` | `go_module_path` | string |
| `ruby.module_name` | `ruby_module_name` | string |
| `ruby.gem_name` | `ruby_gem_name` | string |

### Global section

Options that are not specific to one target live under a `weaveffi`
section (the alias `global` is also accepted):

| Inline (IDL) | TOML field | Type |
|--------------|------------|------|
| `weaveffi.strip_module_prefix` | `strip_module_prefix` | bool |
| `weaveffi.template_dir` | `template_dir` | string |
| `weaveffi.pre_generate` | `pre_generate` | string |
| `weaveffi.post_generate` | `post_generate` | string |

### YAML example

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

Unknown target names (for example a future language target) and
unknown keys inside a known target are silently ignored, so an older
`weaveffi` CLI can still read an IDL written for a newer one.

## Performance

`weaveffi generate` is fast enough that you generally don't need to think
about it, but two design choices keep large multi-generator runs snappy:

### Parallel generator dispatch

The orchestrator dispatches every selected generator in parallel using
[rayon](https://docs.rs/rayon). The pre-generate hook still runs before
any generator starts and the post-generate hook still runs after every
generator finishes — only the codegen step itself is parallelised.

You don't need to opt in or configure anything. Selecting all 11 targets
on a multi-core machine is roughly as fast as running the slowest single
generator alone.

### Per-generator incremental cache

WeaveFFI persists one hash per generator under
`{out_dir}/.weaveffi-cache/{target}.hash`. On every run, each generator's
hash is recomputed (the IR plus the generator's name) and compared
against the persisted value. Only generators whose hashes have changed
are re-executed; the rest are skipped.

This means tweaking a single field that only affects, say, the C header
will only re-run the C generator on the next `weaveffi generate` —
the other 10 generators stay cached.

To clear every cached entry and force a full rebuild, pass `--force`:

```bash
weaveffi generate api.yml -o generated --force
```

The legacy single-file `.weaveffi-cache` written by older CLIs is
ignored on first run and replaced with the new per-generator directory
layout automatically.

## Continuous integration

Two CLI flags are designed for automated pipelines: `weaveffi diff
--check` enforces that committed bindings stay in sync with the IDL,
and `weaveffi validate|lint --format json` produces machine-readable
output that scripts and quality dashboards can consume directly.

### `weaveffi diff --check`

Runs the generator into a temporary directory, compares against the
committed `--out` directory, and exits non-zero if anything would
change. Only a one-line summary is printed; per-file diffs are
suppressed so the log stays small.

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | The committed output matches the IDL exactly. |
| `2` | One or more files would change in place. |
| `3` | One or more files would be added or removed. |

```bash
weaveffi diff api.yml --out generated --check
# Example output:
# + 0 added, - 0 removed, ~ 3 modified
```

A typical "guard" job looks like:

```yaml
# .github/workflows/ci.yml
- name: Verify generated bindings are up to date
  run: weaveffi diff api.yml --out generated --check
```

If the command fails, the contributor must run `weaveffi generate
api.yml -o generated` locally and commit the regenerated files.

### `weaveffi validate --format json`

Emits a single JSON object on stdout. On success:

```json
{ "ok": true, "modules": 2, "functions": 8, "structs": 3, "enums": 1 }
```

On failure, the object lists the structured error so a quality gate
can surface the offending identifier without parsing the human-readable
diagnostic:

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

Pair `--format json` with the global `--quiet` flag to ensure no
stray header lines end up on stdout or stderr — useful when piping
the output straight into `jq` or a CI report parser:

```bash
weaveffi --quiet validate api.yml --format json | jq '.ok'
```

The exit code is `0` on success and non-zero on failure, so existing
shell idioms (`set -e`, `if !`) keep working unchanged.

### `weaveffi lint --format json`

Returns the warning list as a stable JSON document. Every warning has
the same three fields (`code`, `location`, `message`), regardless of
the underlying lint:

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

`ok` is `true` when the warnings array is empty and `false` otherwise,
mirroring the process exit code (`0` clean, `1` warnings present). A
common CI recipe is to fail on regressions but still surface the full
warning list as build metadata:

```bash
weaveffi --quiet lint api.yml --format json > lint-report.json || \
  (cat lint-report.json && exit 1)
```
