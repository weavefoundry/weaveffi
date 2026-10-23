# Project Configuration

A project's settings live in one `weaveffi.toml` at its root. The API
definition (a Rust producer crate or an IDL) describes only the API; this file says
where it is, what the library is called, how it's built and packaged, and how
each target is generated.
Every table is optional, and every unknown table or key is an error, so a typo
fails the run instead of being ignored.

```toml
[project]
input = "."                 # this crate, or an IDL such as "kvstore.yml"
out = "bindings"
targets = ["c", "swift", "kotlin", "python"]

[package]
name = "kvstore"
version = "1.2.0"
description = "An embedded key-value store"
license = "MIT"
authors = ["Example <hello@example.dev>"]
repository = "https://github.com/example/kvstore"

[build]
platforms = ["darwin-arm64", "ios-arm64", "ios-sim-arm64", "android-arm64"]

[generators.kotlin]
name = "com.example.kvstore"
```

`weaveffi init` writes a starter file: in a Rust crate it points `input` at
the crate (`"."`); elsewhere it also writes a starter IDL.

## Discovery

With an explicit input (`weaveffi generate api.yml`, `weaveffi generate
path/to/crate`), the CLI uses the nearest `weaveffi.toml` at or above the
input (for a crate, at or above its directory). With no input, it looks for
`weaveffi.toml` in the current directory and its parents and reads
`[project] input`, so `weaveffi generate` works from anywhere inside the
project, like `cargo build`. `--config <path>` names a file explicitly.
Without any file, every setting takes its default.

## `[project]`

| Key | Default | Meaning |
|-----|---------|---------|
| `input` | none | The API definition: a Rust producer crate (its directory, usually `"."`, or its `Cargo.toml`), whose API is read from its built library (see [Library Mode](extract.md)), or a `.yml`, `.yaml`, or `.json` IDL (TOML is only for this file) |
| `out` | `bindings` | Output directory for `generate` and `dev` |
| `targets` | all eleven | Targets to generate when `--target` isn't given (`weaveffi init` writes `["c", "python"]`) |

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
| `dist` | Where `weaveffi package` writes its artifacts (default `dist`); not part of the identity |

The rules depend on the input:

| | Rust producer (a crate) | IDL |
|-|-----------------------|-----|
| `name` | `[package] name`, else the crate's Cargo package name | `[package] name`, else the input file stem |
| `prefix` | the crate's library name (`[lib] name`, else the package name with `-` mapped to `_`) | `c_prefix`, else snake-case `name` |
| `library` | same as `prefix` (the `cdylib` WeaveFFI builds) | `library`, else snake-case `name` |
| metadata | `[package]`, falling back to `cargo metadata` (workspace-inherited `version.workspace = true` included) | `[package]` |

A Rust producer's prefix is what the macro compiled into its symbols, so it
can't be configured: setting `c_prefix` or `library` for a Rust producer is
an error. Renaming the published package with `name` is fine.

## `[build]`

How `weaveffi build` and `weaveffi package` compile a Rust producer (see
[Packaging](packaging.md)). The commands that build a producer's host
library to read its API (`generate`, `dev`, `validate`, `extract`) use the
`dev` profile unless `--profile` names another; `[build] profile` doesn't
apply to them.

| Key | Default | Meaning |
|-----|---------|---------|
| `platforms` | the host | Platform ids to build when `--platforms` isn't given |
| `profile` | `"release"` | The Cargo profile to build with (`--profile` overrides it) |
| `macos_deployment_target` | `"11.0"` | `MACOSX_DEPLOYMENT_TARGET`, and the macOS version wheel tags carry |
| `ios_deployment_target` | `"13.0"` | `IPHONEOS_DEPLOYMENT_TARGET` |
| `android_api` | `21` | The minimum Android API level, which picks the NDK compiler |
| `manifest` | the project's crate | The producer's `Cargo.toml`, for an IDL project built from a Rust crate |

## `[generators.<target>]`

One table per target (`c`, `cpp`, `swift`, `kotlin`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`) holds that generator's options.
Every target that names something after the package spells the override
`name`:

| Table | `name` sets | Default |
|-------|-------------|---------|
| `[generators.cpp]` | the C++ namespace | `{prefix}` |
| `[generators.swift]` | the SwiftPM package, product, and module (the C module is `C{name}`) | `PascalCase(name)` |
| `[generators.kotlin]` | the Kotlin package | `{prefix}` |
| `[generators.node]` | the npm package | `{name}` |
| `[generators.wasm]` | the npm package | `{name}` |
| `[generators.python]` | the PyPI distribution | `{name}` |
| `[generators.dotnet]` | the namespace, assembly, and NuGet id | `PascalCase(name)` |
| `[generators.dart]` | the pub package | `{prefix}` |
| `[generators.go]` | the module path | `{name}` |
| `[generators.ruby]` | the gem | `{name}` |

The C target has no package name. A few targets name a second thing:

| Table | Key | Default |
|-------|-----|---------|
| `[generators.cpp]` | `header_name` | `{library}.hpp` |
| `[generators.python]` | `import_name` (the import package) | `{prefix}` |
| `[generators.ruby]` | `module_name` (the top-level Ruby module) | `PascalCase(name)` |
| `[generators.go]` | `package` (the Go package name) | `{prefix}` with underscores removed (`kitchen_sink` gives `kitchensink`) |

The constants that go into package manifests are options too:

| Table | Key | Default |
|-------|-----|---------|
| `[generators.swift]` | `min_macos`, `min_ios` (the `platforms:` of `Package.swift`) | `"11.0"`, `"13.0"` |
| `[generators.swift]` | `xcframework_url` (where the packaged binary target downloads from; `{version}` and `{file}` are substituted) | a placeholder |
| `[generators.kotlin]` | `min_sdk`, `compile_sdk` | `21`, `35` |
| `[generators.python]` | `requires_python` | `">=3.10"` |
| `[generators.node]` | `node_engine` (`engines.node`) | `">=18"` |
| `[generators.dart]` | `sdk` (the pubspec SDK constraint) | `">=3.10.0 <4.0.0"` |

Keep `min_macos`, `min_ios`, and `min_sdk` at or above the `[build]`
deployment targets and API level, or the toolchains warn that the library is
newer than the package claims. The remaining options (C's
`buffer_helpers`, C++'s `standard`, and Kotlin's `flavor`) are documented on
the target's page
under [Generators](../generators/README.md). No target has its own C prefix:
the prefix belongs to the library.

## Generation records

Rendering is pure: each target renders its files in memory, and the
orchestrator does every write. Every run renders every selected target, then:

- Only files whose contents changed are rewritten, so file timestamps (and
  incremental builds) stay stable. When nothing changed, nothing is written.
- The orchestrator records the files each target produced in
  `{out}/.weaveffi-cache/{target}.json`. Files the previous run wrote that
  the current run no longer produces (a renamed type, a removed module) are
  deleted. Files you added under the output directory (a `node_modules/`, a
  build directory) are never touched.

`--dry-run` validates and prints every file the targets render, without
writing.

## CI

`weaveffi generate --check` renders in memory and compares with the output
directory without writing anything:

```bash
weaveffi generate --check                       # uses [project]
weaveffi generate path/to/crate -o bindings --target c,swift --check
```

It lists every file that would change (`+` added, `~` modified, `-`
removed, where a stale file from the previous run counts as removed) and
exits `0` when the directory is up to date and `1` otherwise. `weaveffi
generate --diff` prints the same changes as a unified diff, also without
writing (it exits `0`; add `--check` to exit `1` on changes). `--dry-run`
can't be combined with either.
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
  that's always `{library}`. The targets that load the library at run time
  (Python, Ruby, .NET, Dart, Kotlin on the JVM, and WebAssembly on Node.js)
  also accept a full path in the `{PREFIX}_LIBRARY` environment variable;
  C, C++, Swift, Go, and the Node.js addon link it when they're built (see
  each target's page).
