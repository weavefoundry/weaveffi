# Library Mode

The CLI never reads Rust source. A Rust producer's API is read from the
library the crate builds: `#[weaveffi::module]` embeds a description of every
declaration it exports in the compiled library, and `weaveffi generate`,
`diff`, `validate`, `extract`, `dev`, `build`, and `package` read it back
out. The bindings therefore describe exactly what the library was compiled
with, `#[cfg]` and type aliases resolved, macro expansion and all.

A Rust project's input is the crate: its directory (`[project] input = "."`,
which `weaveffi init` writes) or its `Cargo.toml`.

```bash
weaveffi generate                    # build the crate (debug), read it, generate
weaveffi generate --release          # read a release build instead
weaveffi generate --library target/x86_64-unknown-linux-gnu/release/libkvstore.so
```

Without `--library`, the CLI resolves the crate with `cargo metadata` (its
name, version, library name, and target directory), runs
`cargo build --lib --message-format=json-render-diagnostics` (Cargo's errors
and warnings print as usual), and reads the crate's `cdylib`, or its
`staticlib` when it has no `cdylib`. `--library` skips the build and reads
the given file: a shared library, a static archive, or a `.wasm` module, for
any platform (an Android `.so` reads fine on a Mac). With no crate at all (no
input argument and no `[project] input`), `--library` reads the library on
its own: the prefix comes from its metadata and the library name from its
file name, and a file holding the metadata of several WeaveFFI crates is an
error that asks for the crate.

## Extracting an IDL

`weaveffi extract` prints the API the library embeds as an IDL document,
which is useful for reviewing the API surface, committing a canonical IDL
next to the crate, or handing the API to a non-Rust implementation.

```bash
weaveffi extract                     # the project's crate, YAML to stdout
weaveffi extract path/to/crate -o api.yml
weaveffi extract -f json -o api.json # JSON (or -f toml)
weaveffi extract --library libkvstore.dylib
```

| Flag | Default | Meaning |
|------|---------|---------|
| `-o`, `--output` | stdout | Write to a file |
| `-f`, `--format` | `yaml` | `yaml`, `json`, or `toml` |
| `--library` | build the crate | Read this library instead |
| `--release` | off | Build the crate in release mode |
| `--config` | discovered | The `weaveffi.toml` to use |

The document is validated before it's printed, so what `extract` prints
always generates. An IDL input is an error: there's nothing to extract. The
macro validates each module tree as it compiles; the CLI's validation adds
the rules that span trees (names unique across the whole crate, C symbols
that collide between trees).

The extracted IDL carries no package identity. When you generate from it,
the identity comes from `[package]` in `weaveffi.toml` (IDL rules), not from
`Cargo.toml`, so set `name`, `c_prefix`, and `library` to match the crate if
the generated bindings must load the Rust library.

## How the API is embedded

For every declaration (a module, its error domain, a function, an interface
and each of its members, a record, an enum, a callback interface) the macro
emits an exported static:

```rust,ignore
#[unsafe(no_mangle)]
#[cfg_attr(target_family = "wasm", unsafe(link_section = "weaveffi_meta"))]
#[used]
pub static KVSTORE_META_3A5F0C2E9B1D4467: [u8; 141] = *b"\x89\x00\x00\x00{\"schema\":...}";
```

The bytes are a frame: a little-endian `u32` length, then compact JSON that
records the declaration (in the IDL's own shape), the path of its module,
its position among its siblings, the crate's prefix, and the schema version.
The name is the crate's uppercase prefix, `_META_`, and 16 uppercase hex
digits of the FNV-1a 64 hash of the declaration's dotted path (followed by a
`.` for a module, so a module and a declaration of the same name differ), so
it never collides with another declaration or another crate.

The CLI reads native libraries with the `object` crate, walking the symbol
tables (and a PE export table) for `_META_` symbols and reading each one's
bytes from the section that holds it. A `wasm32` module keeps its data out
of reach of any symbol table, so there the frames go to the `weaveffi_meta`
custom section, which the linker fills with every frame back to back. The
CLI keeps the frames carrying the crate's own prefix (a dependency that is
itself a WeaveFFI producer embeds frames too) and assembles them, in
declaration order, into the API. Separate top-level modules are separate
macro invocations, so they're ordered by name.

A static carries its declaration's `#[cfg]`, and sits inside the module it
describes, so the module's `#[cfg]` applies too. A declaration a build
compiles out is missing from that build's metadata exactly as it's missing
from its symbols and its contract table:

```rust
#[weaveffi::module]
pub mod shop {
    #[cfg(feature = "extra")]
    #[weaveffi::export]
    pub fn discount() -> u32 { 10 }
}
```

`weaveffi extract` lists `discount` only for a build with the `extra`
feature (`cargo build --features extra`, then `--library`).

A library with no frames for the crate (the API isn't annotated, or the
crate builds no `cdylib` or `staticlib`) is an error naming what's missing:
`#[weaveffi::module]` on the API and one `weaveffi::export_runtime!()` call.

## Iterating: `weaveffi dev`

`weaveffi dev` builds the crate's debug library, generates, and makes the
generated packages find that library without any environment variables
where a package looks for a bundled copy (when the crate builds a shared
library; a static-only build gets the environment variable instead):

- **Python**: the library is copied into the package directory, which the
  loader checks first.
- **Node.js**: the library is copied next to `binding.gyp`, which links the
  addon against it and records its directory as an rpath, so `npm install`
  in the package builds an addon that loads it.
- **Ruby**: the library is copied to `lib/native/`, which the loader checks
  first.

For every other target, `dev` prints what to set: `{PREFIX}_LIBRARY=<path>`
for the targets that load the library at run time (.NET, Dart, Kotlin), and
the library directory to link with and put on the loader path for the
targets that link at build time (C, C++, Swift, Go). WebAssembly needs a
`wasm32` build, which `dev` doesn't make (`weaveffi build --platforms
wasm32` does). Rerun `weaveffi dev` after changing the crate: copies don't
follow later builds.

## Generating from a build script

The CLI is also a library. `weaveffi_cli::project::Project` locates a
project the way the `weaveffi` command does and generates it, so a crate
that ships bindings can generate them from its `build.rs`:

```rust,ignore
// build.rs
fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-changed=weaveffi.toml");
    println!("cargo::rerun-if-changed=api.yml");
    weaveffi_cli::project::Project::discover(env!("CARGO_MANIFEST_DIR"))?.generate()?;
    Ok(())
}
```

```toml
[build-dependencies]
weaveffi-cli = "0.24"
miette = "7"
```

`Project::discover` finds the nearest `weaveffi.toml` at or above the
directory, and `generate` writes the `[project] targets` into `[project]
out`, rewriting only changed files.

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
| `String`, `&str` | `string` |
| `Vec<u8>`, `&[u8]` | `bytes` |
| `Vec<T>`, `&[T]` | `[T]` |
| `Option<T>` | `T?` |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `{K:V}` |
| `weaveffi::Iter<T>` | `iter<T>` |
| `&T`, `Arc<T>` (interface), `Arc<Self>` | `T` |
| `Arc<dyn Trait>` (callback interface) | `Trait` |
| `weaveffi::CancelToken` | removed (`#[weaveffi::cancellable]` sets `cancellable: true`) |
| `Result<T, E>` | return `T`, sets `throws: true` |
| a type alias declared in the tree | its target |
| any other name | that name, resolved by the validator |

Compositions map recursively: `Option<Vec<i32>>` is `[i32]?` and
`Vec<Arc<Gadget>>` is `[Gadget]`. An `async fn` sets `async: true`, `()` and
`Result<(), E>` returns are no return, and a callback method's
`Result<T, ForeignError>` is a `T` return that throws only with
`#[weaveffi::throws]`. A type is always referenced by its bare name
(`Store`, wherever it's declared), because type names are global; a path
such as `super::kv::Store` contributes only its last segment.

## Limits

- **Error messages.** A `#[weaveffi::error]` variant's doc comment becomes
  the code's `doc:`, and its first line the code's `message:` (the variant
  name when it has no doc), so a Rust producer can't give a code a message
  that differs from its doc's first line.
- **Deprecation versions.** `#[deprecated(since = "...")]` keeps only the
  note; the IDL has no `since` field.
- **Parameter docs.** Rust doesn't allow doc comments on function
  parameters, so an extracted API has no `Param.doc`; describe parameters in
  the function's own doc comment.

`crates/weaveffi-cli/tests/cli/extract.rs` builds the fixture producer in
`crates/weaveffi-cli/tests/fixtures/producer`, checks that `weaveffi
extract` prints exactly its `expected.yml`, and checks that generating from
the library and from that IDL gives identical output.
