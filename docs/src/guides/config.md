# Project Configuration

A project's settings live in one `weaveffi.toml` at its root. The API
definition (annotated Rust or an IDL) describes only the API; this file says
where it is, what the library is called, and how each target is generated.
Every table is optional, and every unknown table or key is an error, so a typo
fails the run instead of being ignored.

```toml
[project]
input = "src/lib.rs"        # or an IDL such as "kvstore.yml"
out = "bindings"
targets = ["c", "swift", "kotlin", "python"]

[package]
name = "kvstore"
version = "1.2.0"
description = "An embedded key-value store"
license = "MIT"
authors = ["Example <hello@example.dev>"]
repository = "https://github.com/example/kvstore"

[global]
post_generate = "swiftformat bindings/swift"

[generators.kotlin]
package = "com.example.kvstore"
```

`weaveffi init` writes a starter file: in a Rust crate it points `input` at
`src/lib.rs`; elsewhere it also writes a starter IDL.

## Discovery

With an explicit input (`weaveffi generate api.yml`), the CLI uses the nearest
`weaveffi.toml` at or above the input's directory. With no input, it looks for
`weaveffi.toml` in the current directory and its parents and reads
`[project] input`, so `weaveffi generate` works from anywhere inside the
project, like `cargo build`. `--config <path>` names a file explicitly.
Without any file, every setting takes its default.

## `[project]`

| Key | Default | Meaning |
|-----|---------|---------|
| `input` | none | The API definition: a `.rs` file or a `.yml`, `.yaml`, `.json`, or `.toml` IDL |
| `out` | `generated` | Output directory for `generate` and `diff` |
| `targets` | all eleven | Targets to generate when `--target` isn't given |

Paths are relative to the directory holding `weaveffi.toml`. Command-line
arguments (`input`, `-o`, `--target`) override the table.

## `[package]`

`[package]` resolves into the library's [identity](../reference/naming.md):
the package `name` every ecosystem publishes under, the C symbol `prefix`,
and the native `library` base name. Generators derive every other name from
these three.

| Key | Meaning |
|-----|---------|
| `name` | Distribution name (may contain `-` or `.`) |
| `version` | Version stamped into every manifest (default `0.1.0`) |
| `description`, `license`, `authors`, `homepage`, `repository` | Manifest metadata |
| `c_prefix` | C symbol prefix; IDL inputs only |
| `library` | Native library base name (`lib{library}.so`); IDL inputs only |

The rules depend on the input:

| | Rust producer (`.rs`) | IDL |
|-|-----------------------|-----|
| `name` | `[package] name`, else the crate's Cargo package name | `[package] name`, else the input file stem |
| `prefix` | the crate's library name (`[lib] name`, else the package name with `-` mapped to `_`) | `c_prefix`, else snake-case `name` |
| `library` | same as `prefix` (the cdylib Cargo builds) | `library`, else snake-case `name` |
| metadata | `[package]`, falling back to `Cargo.toml` | `[package]` |

A Rust producer's prefix is what the macro compiled into its symbols, so it
can't be configured: setting `c_prefix` or `library` for a `.rs` input is an
error. Renaming the published package with `name` is fine.

## `[global]`

| Key | Meaning |
|-----|---------|
| `strip_module_prefix` | Sets `strip_module_prefix` on every target that supports it, overriding their own tables |
| `pre_generate` | Shell command run once before `generate` writes anything |
| `post_generate` | Shell command run once after `generate` finishes writing |

Targets that place every module's members in one flat namespace strip the
module name from free functions by default (`get_stats`, not
`kv_stats_get_stats`). Set `strip_module_prefix = false` when two modules
declare the same function name; targets that namespace each module
separately don't need it. Each language page says which applies.

Hooks run through `sh -c` (`cmd /C` on Windows), only when at least one
target actually regenerates; an up-to-date run skips them. `weaveffi diff`
never runs them. Don't put untrusted input in a hook.

## `[generators.<target>]`

One table per target (`c`, `cpp`, `swift`, `kotlin`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`) holds that generator's options.
These keys override names derived from the identity:

| Table | Key | Default |
|-------|-----|---------|
| `[generators.cpp]` | `namespace`, `header_name` | `{prefix}`, `{library}.hpp` |
| `[generators.swift]` | `module_name` | `PascalCase(name)` |
| `[generators.kotlin]` | `package` | `{prefix}` |
| `[generators.node]` | `package_name` | `{name}` |
| `[generators.wasm]` | `package_name` | `{name}` |
| `[generators.python]` | `package_name`, `import_name` | `{name}`, `{prefix}` |
| `[generators.dotnet]` | `namespace` | `PascalCase(name)` |
| `[generators.dart]` | `package_name` | `{prefix}` |
| `[generators.go]` | `module_path` | `{name}` |
| `[generators.ruby]` | `gem_name`, `module_name` | `{name}`, `PascalCase(name)` |

Every other option (Kotlin's Android or JVM flavor, Wasm's Emscripten mode,
and so on) is documented on the target's page under
[Generators](../generators/README.md). No target has its own C prefix: the
prefix belongs to the library.

## Generation and caching

Rendering is pure: each target renders its files in memory, and the
orchestrator does every write. After a target generates, the orchestrator
writes a record to `{out}/.weaveffi-cache/{target}.json` holding a hash of
every input that affects the output (the API, the identity, the target's
config, and the CLI version) and the path and hash of every file written. On
the next run:

- A target is skipped when its input hash matches and every recorded file is
  still on disk unmodified. Deleting or hand-editing a generated file
  regenerates it.
- A regenerated target rewrites only files whose contents changed, so file
  timestamps (and incremental builds) stay stable.
- Files the previous run wrote that the current run no longer produces (a
  renamed type, a removed module) are deleted. Files you added under the
  output directory (a `node_modules/`, a build directory) are never touched.

`--force` ignores the records and regenerates every selected target.
`--dry-run` validates and prints the files that would be written.

## CI

`weaveffi diff --check` regenerates in memory and compares with the output
directory without writing anything or running hooks:

```bash
weaveffi diff --check                       # uses [project]
weaveffi diff src/lib.rs -o bindings --target c,swift --check
```

It exits `0` when the directory is up to date, `2` when files would change,
and `3` when files would be added or removed (stale files count as removed).
`weaveffi validate --format json` prints one JSON object with `ok`, counts,
and any errors (each with a `code` matching the
[error catalog](../reference/idl.md#error-catalog)); `--warn` adds advisory
warnings, which never change the exit status.

## Pitfalls

- **Config travels with the definition.** Two checkouts with different
  `weaveffi.toml` files generate different packages from the same source.
  Commit the file next to the definition and let discovery find it.
- **Name overrides don't rename the library.** A target's package or module
  override changes what consumers import, not which native library it loads;
  that's always `{library}`, overridable at run time with the
  `{PREFIX}_LIBRARY` environment variable.
