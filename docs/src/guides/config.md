# Project Configuration

## Overview

WeaveFFI ships with sensible defaults so `weaveffi generate src/lib.rs` (or
`weaveffi generate api.yml`) just works. The API definition, whether it's an
annotated Rust module or an IDL document, describes only the API. Everything
about how that API is published lives in one optional file next to it:
`weaveffi.toml`.

```toml
[package]
name = "kvstore"
version = "1.2.0"
description = "An embedded key-value store"
license = "MIT"
authors = ["Example <hello@example.dev>"]
repository = "https://github.com/example/kvstore"

[global]
c_prefix = "kv"

[generators.swift]
module_name = "KVStore"

[generators.android]
package = "com.example.kvstore"
```

The file has three tables, all optional:

- `[package]`: the distribution identity stamped into every generated
  manifest (`package.json`, `pyproject.toml`, `Package.swift`, `.csproj`,
  `pubspec.yaml`, `go.mod`, `.gemspec`, and so on).
- `[global]`: knobs that affect several generators or the orchestrator
  itself.
- `[generators.<target>]`: one table per language with that generator's own
  options.

## Discovery

`weaveffi generate`, `weaveffi package`, and `weaveffi diff` find the config
the same way Cargo finds `Cargo.toml`: they walk up from the input file's
directory and use the first `weaveffi.toml` they meet. Keeping the file beside
`Cargo.toml` (for a Rust producer) or beside the IDL therefore needs no flag.

```bash
weaveffi generate src/lib.rs -o generated                 # nearest weaveffi.toml
weaveffi generate src/lib.rs -o generated --config ci.toml # a specific file
```

Without any config file, every generator runs with its defaults and the
package name falls back as described under [Package identity](#package-identity).

Unknown tables and keys are rejected, so a typo like `[generator.swift]` or
`modulename` fails the run instead of silently doing nothing.

## `[package]`

| Key           | Type       | Description                                                      |
|---------------|------------|------------------------------------------------------------------|
| `name`        | string     | Distribution name; normalized per ecosystem (see below)          |
| `version`     | string     | Published version (default `0.1.0`)                              |
| `description` | string     | One-line description for manifests that carry one                |
| `license`     | string     | SPDX license expression                                          |
| `authors`     | `[string]` | Author strings, `Name <email>`                                   |
| `homepage`    | string     | Project home page URL                                            |
| `repository`  | string     | Source repository URL                                            |

### Package identity

Every generated manifest resolves its name through one shared policy. For
an identity value, an explicit per-target key wins; otherwise the generator
uses `package.name` normalized for its ecosystem (`kvstore` becomes
`Kvstore` for Swift and .NET, `kvstore` for Python and Ruby, and so on);
otherwise it falls back to the input file stem, and finally to the
`weaveffi`/`WeaveFFI` default. The per-target keys that participate are
`[generators.swift] module_name`, `[generators.node] package_name`,
`[generators.python] package_name`, `[generators.dart] package_name`,
`[generators.go] module_path`, `[generators.ruby] gem_name`, and
`[generators.dotnet] namespace` (which also sets the NuGet package id).

For a Rust producer whose input is `src/lib.rs`, the input file stem (`lib`)
is useless as a name, so the CLI uses the crate directory name instead
(`my-crate/src/lib.rs` yields `my-crate`) when `[package] name` is unset.

## `[global]`

| Key                   | Type   | Default | Description                                                                                                                                                        |
|-----------------------|--------|---------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `c_prefix`            | string | unset   | The C ABI symbol prefix (`{prefix}_{module}_{function}`), fanned out to every target that hasn't set its own `prefix`; wins over `[generators.c] prefix`               |
| `strip_module_prefix` | bool   | unset   | Sets `strip_module_prefix` on every target that supports it, overriding their tables. Stripping is on by default, so `false` restores module-prefixed names everywhere |
| `pre_generate`        | string | none    | Shell command run once before any generator starts                                                                                                                 |
| `post_generate`       | string | none    | Shell command run once after every generator finishes                                                                                                              |

The C ABI symbol prefix is global by nature: every consumer must call the
identical exported symbols. The CLI resolves it once (`[global] c_prefix`,
then `[generators.c] prefix`) and fans it out to every per-target config
that leaves `prefix` unset, so a custom prefix is honored across all eleven
languages, not just C and C++. The generated C header aliases the runtime
symbols (`{prefix}_free_string`, `{prefix}_error_clear`, ...) back to the
canonical `weaveffi_*` names the `weaveffi-abi` runtime exports, so a Rust
producer needs no change.

## `[generators.<target>]`

Every table also accepts a `prefix` key naming the C ABI symbol prefix its
wrappers call; you rarely set it per target because of the fan-out above.

| Table                  | Key                   | Type   | Default           | Description                                                                                                                                                                               |
|------------------------|-----------------------|--------|-------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `[generators.c]`       | `prefix`              | string | `"weaveffi"`      | Prefix prepended to every C ABI symbol                                                                                                                                                    |
| `[generators.cpp]`     | `namespace`           | string | `"weaveffi"`      | C++ namespace for the wrapper                                                                                                                                                             |
| `[generators.cpp]`     | `header_name`         | string | `"weaveffi.hpp"`  | Header file name for the C++ output                                                                                                                                                       |
| `[generators.cpp]`     | `standard`            | string | `"17"`            | C++ standard for the generated `CMakeLists.txt`                                                                                                                                           |
| `[generators.swift]`   | `module_name`         | string | identity          | Swift module name in `Package.swift` and the `Sources/` directory                                                                                                                          |
| `[generators.swift]`   | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Swift symbols                                                                                                                                        |
| `[generators.android]` | `package`             | string | `"com.weaveffi"`  | Java/Kotlin package declaration in the JNI wrapper                                                                                                                                        |
| `[generators.android]` | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Kotlin symbols                                                                                                                                       |
| `[generators.node]`    | `package_name`        | string | identity          | npm package name                                                                                                                                                                          |
| `[generators.node]`    | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted JS/TS symbols                                                                                                                                        |
| `[generators.wasm]`    | `module_name`         | string | `"weaveffi_wasm"` | Module name in the Wasm JS loader                                                                                                                                                         |
| `[generators.wasm]`    | `emscripten`          | bool   | `false`           | Target an Emscripten build: the loader accepts a pre-initialized Emscripten `Module` (or its `MODULARIZE` factory promise) instead of a `.wasm` URL; async, callbacks, and listeners become throwing stubs |
| `[generators.python]`  | `package_name`        | string | identity          | Python package name                                                                                                                                                                       |
| `[generators.python]`  | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Python symbols                                                                                                                                       |
| `[generators.dotnet]`  | `namespace`           | string | identity          | .NET namespace and NuGet package id                                                                                                                                                       |
| `[generators.dotnet]`  | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted C# symbols                                                                                                                                           |
| `[generators.dart]`    | `package_name`        | string | identity          | Dart package name in `pubspec.yaml`                                                                                                                                                       |
| `[generators.dart]`    | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Dart symbols                                                                                                                                         |
| `[generators.go]`      | `module_path`         | string | identity          | Go module path in `go.mod`                                                                                                                                                                |
| `[generators.go]`      | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Go symbols                                                                                                                                           |
| `[generators.ruby]`    | `module_name`         | string | `"WeaveFFI"`      | Ruby module that wraps the bindings                                                                                                                                                       |
| `[generators.ruby]`    | `gem_name`            | string | identity          | Ruby gem name                                                                                                                                                                             |
| `[generators.ruby]`    | `strip_module_prefix` | bool   | `true`            | Strip the module prefix from emitted Ruby symbols                                                                                                                                         |

"identity" means the value follows [Package identity](#package-identity).

## Recipes

```toml
# iOS / macOS app with a branded module and symbol prefix
[global]
c_prefix = "myapp"

[generators.swift]
module_name = "MyAppFFI"
```

```toml
# Android library
[generators.android]
package = "com.example.myapp.ffi"
```

```toml
# Scoped npm package, module-prefixed names everywhere
[global]
strip_module_prefix = false

[generators.node]
package_name = "@myorg/myapp-native"
```

```toml
# Build the producer before generating so `weaveffi package --build` can
# find fresh binaries
[global]
pre_generate = "cargo build --release"
```

## Wiring it into CI

`weaveffi diff --check` enforces that the committed bindings still match the
API definition and config. A typical guard job:

```yaml
# .github/workflows/ci.yml
- name: Verify generated bindings are up to date
  run: weaveffi diff src/lib.rs --out generated --check
```

Exit codes:

| Code | Meaning                                              |
|------|------------------------------------------------------|
| `0`  | The committed output matches the definition exactly. |
| `2`  | One or more files would change in place.             |
| `3`  | One or more files would be added or removed.         |

`weaveffi validate --format json` emits structured success or failure, and
`--warn` adds the advisory lint warnings to the same document:

```bash
weaveffi --quiet validate src/lib.rs --warn --format json | jq '.ok, .warnings'
```

```json
{ "ok": true, "modules": 2, "functions": 8, "structs": 3, "enums": 1, "warnings": [] }
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

Warnings never change `ok` or the exit status; they carry stable `code`,
`location`, and `message` fields for dashboards to key on.

## Performance and caching

- The orchestrator dispatches every selected generator in parallel using
  [rayon](https://docs.rs/rayon). The `pre_generate` and `post_generate`
  hooks run serially around the whole batch.
- Each generator persists a hash under
  `{out_dir}/.weaveffi-cache/{target}.hash` covering the resolved API
  (including `[package]`), the generator's name and config, and the CLI
  version. Only generators whose hash changed re-run; pass `--force` to
  invalidate every entry.

## Pitfalls

- **The C prefix rewrites every generator**: picking a custom prefix
  renames every exported business symbol. Rust producers using
  `#[weaveffi::module]` must be built with the same prefix; every wrapper
  picks it up from the resolved global value, so if you also set a
  per-target `prefix`, make sure they agree.
- **Module-prefix stripping flattens names**: it's on by default, so two
  modules that each declare an `open` function collide in targets with a
  flat namespace. Rename one, or set `strip_module_prefix = false`
  (globally or per target) to restore prefixed names.
- **Hooks run shell commands as-is**: `pre_generate` and `post_generate`
  are passed straight to `sh -c`. Quote them carefully and never include
  untrusted input.
- **Config lives outside the definition**: two checkouts generating from
  the same `lib.rs` with different `weaveffi.toml` files produce different
  manifests. Commit the config next to the definition and let discovery
  find it rather than passing `--config` from scripts.
