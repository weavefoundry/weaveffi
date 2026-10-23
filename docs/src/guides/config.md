# Project Configuration

A project's settings live in one `weaveffi.toml` at its root. The API
definition (a Rust producer crate or an IDL) describes only the API; this
file says where it is, what the library is called, how it's built and
packaged, and how each target is generated. Every table is optional, and
every unknown table or key is an error, so a typo fails the run instead of
being ignored.

```toml
[project]
input = "."                 # this crate, or an IDL such as "kvstore.yml"
out = "bindings"
targets = ["c", "swift", "kotlin", "python"]

[package]
version = "1.2.0"
description = "An embedded key-value store"
license = "MIT"
authors = ["Example <hello@example.dev>"]
repository = "https://github.com/example/kvstore"

[build]
platforms = ["darwin-arm64", "ios-arm64", "ios-sim-arm64", "android-arm64"]
features = ["sync"]

[generators.swift]
name = "KVStore"
```

`weaveffi init` writes a starter file: in a Rust crate it points `input` at
the crate (`"."`); elsewhere it also writes a starter IDL.

## Discovery

With an explicit input (`weaveffi generate api.yml`, `weaveffi generate
path/to/crate`), the CLI uses the nearest `weaveffi.toml` at or above the
input. With no input, it looks for `weaveffi.toml` in the current directory
and its parents and reads `[project] input`, so `weaveffi generate` works
from anywhere inside the project, like `cargo build`. `--config <path>`
names a file explicitly. Without any file, every setting takes its default.

## `[project]`

| Key | Default | Meaning |
|-----|---------|---------|
| `input` | none | The API definition: a Rust producer crate (its directory, usually `"."`, or its `Cargo.toml`), whose API is read from its built library (see [Library Mode](extract.md)), or a `.yml`, `.yaml`, or `.json` IDL (TOML is only for this file) |
| `out` | `bindings` | Output directory for `generate` |
| `targets` | every default target | Targets to generate when `--target` isn't given (`weaveffi init` writes `["c", "python"]`) |

Paths are relative to the directory holding `weaveffi.toml`. Command-line
arguments (the input, `-o`/`--out`, `-t`/`--target`) override the table. A
`targets` entry that isn't a target is an error, so a typo can't silently
generate nothing.

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

A Rust producer's prefix is what the attributes compiled into its symbols,
so it can't be configured: setting `c_prefix` or `library` for a Rust
producer is an error. Renaming the published package with `name` is fine.

## `[build]`

How a Rust producer is compiled. `profile`, the deployment targets, and
`platforms` apply to `weaveffi build` and `weaveffi package` (see
[Packaging](packaging.md)); the commands that build a producer's host
library to read its API (`generate`, `validate`, and `extract`) use the
`dev` profile unless `--profile` names another. The feature keys apply to
every build.

| Key | Default | Meaning |
|-----|---------|---------|
| `features` | none | Cargo features every build of the producer enables |
| `all_features` | `false` | Build the producer with `--all-features` |
| `no_default_features` | `false` | Build the producer with `--no-default-features` |
| `platforms` | the host | Platform ids to build when `--platforms` isn't given |
| `profile` | `"release"` | The Cargo profile `build` and `package` use (`--profile` overrides it) |
| `macos_deployment_target` | `"11.0"` | `MACOSX_DEPLOYMENT_TARGET`, and the macOS version wheel tags carry |
| `ios_deployment_target` | `"13.0"` | `IPHONEOS_DEPLOYMENT_TARGET` |
| `android_api` | `21` | The minimum Android API level, which picks the NDK compiler |
| `manifest` | the project's crate | The producer's `Cargo.toml`, for an IDL project built from a Rust crate |

**Features reach every build.** `-F`/`--features`, `--all-features`, and
`--no-default-features` on `generate`, `validate`, `extract`, `build`, or
`package` replace the three feature keys, and whichever set applies is
passed to every build of the producer: the host build library mode reads
the API from as much as each platform's build. So a declaration behind
`#[cfg(feature = "...")]` is in the bindings exactly when it's in the
libraries they load.

## `[generators.<target>]`

One table per target (`c`, `cpp`, `swift`, `kotlin`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`) holds that generator's options,
and each target's page under [Generators](../generators/README.md) lists its
keys. Every target that names its package after the library spells the
override `name` (the Swift package and module, the npm package, the .NET
namespace, the Go module path, and so on). Name overrides never change which
native library a package loads; that's always `{library}`. No target has a
C prefix of its own: the prefix belongs to the library.

The C target has one key:

| Key | Default | Meaning |
|-----|---------|---------|
| `buffer_helpers` | `true` | Also emit `{library}_buffer.h`, with a C struct and codec for every value-buffer type, when the API passes any value buffers |

## Generation records

Rendering is pure: each target renders its files in memory, and the
orchestrator does every write. Every run renders every selected target, then:

- Only files whose contents changed are rewritten, so file timestamps (and
  incremental builds) stay stable. When nothing changed, nothing is written.
- The orchestrator records the files each target produced in
  `{out}/.weaveffi/{target}.json`. Files the previous run recorded that the
  current run no longer produces (a renamed type, a removed function) are
  deleted. Files you added under the output directory (a `node_modules/`, a
  build directory) are never touched, and a record entry outside its
  target's own directory is ignored.

Commit the records with the generated bindings, so `generate --check` on a
fresh checkout sees stale files too. `--dry-run` validates and prints every
file the targets render, without writing.

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
and `--dev` can't be combined with either. `weaveffi validate --format json`
prints one JSON object with the result, counts, and any errors (each with a
`code` matching the [error catalog](../reference/idl.md#error-catalog));
`--warn` adds advisory warnings, which never change the exit status.

## Pitfalls

- **Config travels with the definition.** Two checkouts with different
  `weaveffi.toml` files generate different packages from the same source.
  Commit the file next to the definition and let discovery find it.
- **Features on one command only.** Building the libraries you ship with a
  feature that `generate` didn't have (or the reverse) gives bindings that
  don't match them. Put shipped features in `[build] features` so every
  command sees the same set.
- **Name overrides don't rename the library.** The targets that load the
  library at run time also accept a full path in the `{PREFIX}_LIBRARY`
  environment variable; the targets that link it (C, C++, Swift, and Go)
  find it when they're built (see each target's page).
