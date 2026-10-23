# Library Mode

The CLI never reads Rust source. A Rust producer's API is read from the
library the crate builds: each WeaveFFI attribute embeds a description of its
declaration in the compiled library, and `weaveffi generate`, `validate`,
`extract`, `build`, and `package` read it back out. The bindings therefore
describe exactly what the library was compiled with: `#[cfg]`, Cargo
features, type aliases, and macro expansion all resolved.

A Rust project's input is the crate: its directory (`[project] input = "."`,
which `weaveffi init` writes) or its `Cargo.toml`.

```bash
weaveffi generate                    # build the crate (dev profile), read it, generate
weaveffi generate --profile release  # read a release build instead
weaveffi generate -F sync,metrics    # build it with Cargo features
weaveffi generate --library target/x86_64-unknown-linux-gnu/release/libkvstore.so
```

Without `--library`, the CLI resolves the crate with `cargo metadata` (its
name, version, library name, and target directory), runs `cargo rustc --lib
--crate-type cdylib --profile <profile>` with the project's Cargo features
(Cargo's errors and warnings print as usual), and reads the `cdylib` it
built, so the crate needs no `crate-type` of its own. `--profile` defaults
to `dev`, and the features are `-F`/`--features`, `--all-features`, and
`--no-default-features`, else `[build] features`, `all_features`, and
`no_default_features` (see [Project Configuration](config.md#build)): the
same set every build of the producer uses, so the bindings and the shipped
libraries agree on every `#[cfg(feature = "...")]`.

`--library` skips the build and reads the given file: a shared library, a
static archive, or a `.wasm` module, for any platform (an Android `.so`
reads fine on a Mac). With no crate at all (no input argument and no
`[project] input`), `--library` reads the library on its own: the prefix
comes from its metadata and the library name from its file name, and a file
holding the metadata of several WeaveFFI crates is an error that asks for
the crate.

## Extracting an IDL

`weaveffi extract` prints the API the library embeds as an IDL document,
which is useful for reviewing the API surface, committing a canonical IDL
next to the crate, or handing the API to a non-Rust implementation.

```bash
weaveffi extract                       # the project's crate, YAML to stdout
weaveffi extract path/to/crate -o api.yml
weaveffi extract -f json -o api.json   # JSON
weaveffi extract --library libkvstore.dylib
```

| Flag | Default | Meaning |
|------|---------|---------|
| `-o`, `--out` | stdout | Write to a file |
| `-f`, `--format` | `yaml` | `yaml` or `json` |
| `--library` | build the crate | Read this library instead |
| `--profile` | `dev` | The Cargo profile to build the crate with |
| `-F`, `--features`; `--all-features`; `--no-default-features` | `[build]`'s | The Cargo features to build the crate with |
| `--config` | discovered | The `weaveffi.toml` to use |

The document is validated before it's printed, so what `extract` prints
always generates. An IDL input is an error: there's nothing to extract.

The extracted IDL carries no package identity. When you generate from it,
the identity comes from `[package]` in `weaveffi.toml` (IDL rules), not from
`Cargo.toml`, so set `name`, `c_prefix`, and `library` to match the crate if
the generated bindings must load the Rust library.

## How the API is embedded

For every declaration (a function, an interface and each of its members, a
record, an enum, an error domain, a callback interface) the attribute emits
an exported static holding one **frame**, a little-endian `u32` length and
then compact JSON recording the declaration (in the IDL's own shape), the
source location of its attribute, the crate's prefix, the schema version,
and the ABI revision. The static is named the crate's uppercase prefix,
`_META_`, and 16 uppercase hex digits of the FNV-1a 64 hash of the
declaration's path (`Book.add`), so it never collides with another
declaration or another crate:

```rust,ignore
#[unsafe(export_name = "MACRO_TOUR_META_...")]
#[cfg_attr(target_family = "wasm", unsafe(link_section = "weaveffi_meta"))]
#[used]
pub static __WEAVEFFI_META: [u8; N] = /* the frame */;
```

The CLI reads native libraries with the `object` crate, walking the symbol
tables for `_META_` symbols and reading each one's bytes from the section
that holds it (every member of a static archive included). A `wasm32`
module keeps its data out of reach of any symbol table, so there the frames
go to the `weaveffi_meta` custom section, which the linker fills with every
frame back to back. The CLI keeps the frames carrying the crate's own
prefix (a dependency that is itself a WeaveFFI producer embeds frames too)
and assembles them into the API, ordering each kind of declaration by where
its attribute is written (file, line, then column), whatever order the
linker laid the statics out in. A frame built by another WeaveFFI version
(another schema version or ABI revision) is an error.

A frame carries its declaration's `#[cfg]` (and, for an interface member,
its `impl` block's), so a declaration a build compiles out is missing from
that build's metadata exactly as it's missing from its symbols and its
contract functions. The [macro tour](producer-macro.md#conditional-compilation-and-features)'s
`Book::purge` is in the API only when the crate is built with its `admin`
feature (`weaveffi extract -F admin`).

A library with no frames for the crate (the API isn't tagged) is an error
that says what's missing: tagged items and one `weaveffi::export_runtime!()`
call.

## Iterating: `generate --dev`

`weaveffi generate --dev` generates as usual, then points the generated
packages at the library the API was read from (the one it just built, or
`--library`'s):

- it copies the library into each generated package that looks for a
  bundled copy (when the library is a shared library), so it loads with no
  environment variables;
- for every other target that loads the library at run time, it prints the
  `{PREFIX}_LIBRARY=<path>` to set;
- for the targets that link the library when they're built (C, C++, Swift,
  and Go), it prints the directory to link with (`-L<dir> -l{library}`) and
  to put on the loader path;
- WebAssembly needs a `wasm32` build, which `--dev` doesn't make (`weaveffi
  build --platforms wasm32` does).

`--dev` needs a Rust producer (or `--library`), and can't be combined with
`--check`, `--diff`, or `--dry-run`. Rerun it after changing the crate:
copies don't follow later builds. Each target's page says where its package
looks for the library.

## Generating from a build script

The CLI is also a library. `weaveffi_cli::project::Project` locates a
project the way the `weaveffi` command does, and the
`weaveffi_cli::codegen::Orchestrator` generates it, so a crate that ships
bindings for an IDL-defined library can generate them from its `build.rs`:

```rust,ignore
// build.rs
use weaveffi_cli::codegen::Orchestrator;
use weaveffi_cli::project::Project;

fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-changed=weaveffi.toml");
    println!("cargo::rerun-if-changed=api.yml");
    let project = Project::discover(env!("CARGO_MANIFEST_DIR"))?;
    let targets = project.config.targets(None)?;
    Orchestrator::new()
        .with_targets(targets.iter().map(AsRef::as_ref))
        .run(&project.model()?, &project.config.out_dir(None))?;
    Ok(())
}
```

```toml
[build-dependencies]
weaveffi-cli = "0.25"
miette = "7"
```

`Project::discover` finds the nearest `weaveffi.toml` at or above the
directory, and the orchestrator writes the `[project] targets` into
`[project] out`, rewriting only changed files.

A build script runs before its crate is compiled, so it can't read its own
crate's library: from a `build.rs`, the project's input must be an IDL, or
the API must come from a library built earlier, named with
`Project::library(path)`. Pointing a crate's own `build.rs` at the crate is
an error that says so. For a Rust producer, generate after the build
instead: `weaveffi generate` in CI or a `cargo xtask`.

## Type mapping

| Rust | IDL |
|------|-----|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same |
| `usize`, `isize` | `u64`, `i64` |
| `char` | `string` |
| `String`, `&str` | `string` |
| `Vec<u8>`, `&[u8]` | `bytes` |
| `Vec<T>`, `&[T]` | `[T]` |
| `Option<T>`, `Option<&T>` | `T?` |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `{K:V}` |
| `weaveffi::Iter<T>` | `iter<T>` |
| `&T`, `Arc<T>` (interface), `Self`, `Arc<Self>` | `T` |
| `Arc<dyn Trait>` (callback interface) | `Trait` |
| `weaveffi::CancelToken` (an `async fn`'s last parameter) | removed; sets `cancellable: true` |
| `Result<T, E>`, `E` a `#[weaveffi::error]` domain | return `T`, `throws: E` |
| `Result<T, E>`, `E` an untyped error | return `T`, `throws: any` |
| a `#[weaveffi::custom]` type | its repr |
| a type alias | its target |
| a tagged record, enum, or interface | its name |

Compositions map recursively: `Option<Vec<i32>>` is `[i32]?` and
`Vec<Arc<Book>>` is `[Book]`. An `async fn` sets `async: true`, and `()`
and `Result<(), E>` returns are no return. A callback method's `Result<T,
E>` follows the same rule as a function's: a domain `E` throws that domain,
and an untyped one (`ForeignError` included) throws `any`. A type is named
in the IDL by its declaration's name alone, wherever in the crate it's
declared.

## Limits

- **Error messages.** A `#[weaveffi::error]` variant's doc comment becomes
  the code's `doc:`, and the code's `message:` is the variant's
  `#[weaveffi(message = "...")]` template as written, else its doc
  comment's first line, else the variant name. The runtime message is the
  generated `Display` (the template filled from the fields), which the IDL
  can't express.
- **Deprecation versions.** `#[deprecated(since = "...")]` keeps only the
  note; the IDL has no `since` field.
- **Parameter docs.** Rust doesn't allow doc comments on function
  parameters, so an extracted API has no parameter `doc`; describe
  parameters in the function's own doc comment.

The CLI's tests build the fixture producer in
`crates/weaveffi-cli/tests/fixtures/producer`, check that `weaveffi extract`
prints exactly its `expected.yml`, and check that generating from the
library and from that IDL gives identical output.
