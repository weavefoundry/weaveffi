# Getting Started

This guide builds a small Rust library, generates bindings for it, and calls
it from C and from Python. A second, shorter path at the end starts from an
IDL instead and implements the generated C header in C. The code on this
page is the [`examples/getting-started`](https://github.com/weavefoundry/weaveffi/tree/main/examples/getting-started)
crate and the [`examples/idl/greeter`](https://github.com/weavefoundry/weaveffi/tree/main/examples/idl/greeter)
project, which the workspace's tests build and call.

## Install the CLI

You need a stable [Rust toolchain](https://rustup.rs/). Install the
`weaveffi` CLI from crates.io (or grab a prebuilt binary with
`cargo binstall weaveffi-cli`):

```bash
cargo install weaveffi-cli
weaveffi --version
```

## Write the producer

Create a library crate and add the `weaveffi` crate. It needs no
`crate-type`: WeaveFFI builds the C dynamic library itself (with `cargo
rustc --crate-type cdylib`).

```bash
cargo new --lib mathlib
cd mathlib
cargo add weaveffi
```

Replace `src/lib.rs` with this. Everything here is safe Rust; the attributes
write the `extern "C"` layer.

```rust,ignore
{{#include ../../examples/getting-started/src/lib.rs:lib}}
```

A few things to notice:

- Every C symbol starts with the crate's name, so this crate exports
  `mathlib_add`, `mathlib_div`, and `mathlib_greet`, plus the runtime
  (`mathlib_error_clear`, `mathlib_run_free`, and so on). A library is one
  namespace, so a symbol is the prefix and the item's name, wherever in the
  crate the item is declared.
- `Result<i32, MathError>` makes `div` throw the `MathError` domain. The
  enum's discriminants are the stable error codes, and the macro generates
  its `Display` from each variant's doc comment (or a
  `#[weaveffi(message = "...")]` template), which is the message consumers
  see. A `Result` whose error type is an untyped error (`String`,
  `std::io::Error`, `weaveffi::Error`) works too: it throws a message
  without a code to match on.
- `weaveffi::export_runtime!()` appears exactly once, at the crate root.

The [producer macro guide](guides/producer-macro.md) covers records, enums,
objects, callbacks, iterators, async functions, and custom types.

## Configure and generate

Run `weaveffi init` in the crate. It writes a `weaveffi.toml` whose
`[project]` table points at the crate itself, then lists anything the crate
still needs (the `weaveffi` dependency, an exported item, the
`export_runtime!` call):

```bash
weaveffi init
```

The file it writes generates the C header and the Python package; edit
`targets` to pick the languages you ship (without it, every target is
generated):

```toml
{{#include ../../examples/getting-started/weaveffi.toml:config}}
```

Generate the bindings. With a `[project]` table, `weaveffi generate` needs
no arguments and works from any directory in the project:

```bash
weaveffi generate
```

`weaveffi generate` builds the library with Cargo's `dev` profile (pass
`--profile <name>` for another, and `-F`/`--features` for Cargo features),
then reads the API from the library it built: the attributes embed a
description of every exported declaration in the library, so the bindings
match the compiled code exactly, `#[cfg]` included. `weaveffi extract`
prints that description as an IDL.

The output has one directory per target: the C target writes
`bindings/c/mathlib.h`, and the Python target writes an installable package
named `mathlib`. Run `weaveffi generate` again after any change: it rewrites
only the files whose contents changed and removes files a previous run wrote
that it no longer produces, using the generation records it keeps in
`bindings/.weaveffi/` (commit them with the bindings).

## Call it from C

The header is the contract every other binding is built on. Each value is
one C value: a string is a `mathlib_run` (a pointer and a byte length, never
NUL-terminated), every call overwrites a `mathlib_error` the caller provides,
and a returned run is released with `mathlib_run_free`:

```c
{{#include ../../examples/getting-started/main.c}}
```

Build it against the library `weaveffi generate` built:

```bash
cc -I bindings/c main.c -L target/debug -lmathlib -o main
DYLD_LIBRARY_PATH=target/debug ./main    # LD_LIBRARY_PATH on Linux
```

```text
2 + 3 = 5
error 1: division by zero
Hello, C!
```

The first check is the one every generated binding makes for you when it
loads the library: `mathlib_abi_version()` must equal the header's
`MATHLIB_ABI_VERSION`, and `mathlib_contract_check()` returns the name of
the first declaration the library has changed since the header was
generated, or `NULL`. [Errors and Memory](guides/errors-and-memory.md) states
every ownership rule the generated bindings follow for you.

## Call it from Python

While you iterate, run `weaveffi generate --dev`: it generates as usual,
then copies the library it built into the generated packages that look for
a bundled copy (the Python package does) and prints how the other targets
find it. Then install the package:

```bash
weaveffi generate --dev
pip install ./bindings/python
```

```python
import mathlib

print(mathlib.add(2, 3))          # 5
print(mathlib.greet("Python"))    # Hello, Python!

try:
    mathlib.div(1, 0)
except mathlib.MathError as e:
    print(e)                      # (1) division by zero
```

On import the package checks the library's ABI revision and every
declaration's contract, so a library built from incompatible source fails to
load with an error naming the declaration that's missing or changed. To load
a library from somewhere else, set `MATHLIB_LIBRARY` to its full path; every
target that loads the library at run time honors that `{PREFIX}_LIBRARY`
variable. The [Python page](generators/python.md) documents the generated
surface.

## Ship it

```bash
weaveffi package --target python
```

`weaveffi package` builds the library in release mode for this machine
(that's `weaveffi build`), then writes installable artifacts to `dist/`:
here a wheel with the library inside, which needs neither `MATHLIB_LIBRARY`
nor a compiler:

```bash
pip install dist/python/mathlib-*.whl
```

[Packaging](guides/packaging.md) covers more platforms (iOS, Android,
Windows, `wasm32`), every target's artifacts, and publishing.

## Implementing an IDL in C

A producer doesn't have to be Rust. Start from an IDL, generate the C header,
and implement it in any language that can export C symbols. Outside a Rust
crate, `weaveffi init` writes a starter IDL and a `weaveffi.toml`:

```bash
mkdir greeter && cd greeter
weaveffi init           # writes greeter.yml and weaveffi.toml
```

Edit `greeter.yml` to declare the API (the [IDL reference](reference/idl.md)
has the full schema):

```yaml
{{#include ../../examples/idl/greeter/greeter.yml}}
```

For an IDL the identity comes from `[package]` in `weaveffi.toml`, so the
prefix and the library are both `greeter`:

```toml
{{#include ../../examples/idl/greeter/weaveffi.toml:config}}
```

Generate the header and implement it:

```bash
weaveffi generate --target c     # writes bindings/c/greeter.h
```

The API's own functions come first. `GREETER_DEFINE_CONTRACT` defines every
contract function with the hashes the header was generated with, and a
synchronous function stores a cleared error in `out_err` on success:

```c
{{#include ../../examples/idl/greeter/greeter.c:api}}
```

Every producer also exports the runtime surface: the ABI revision, one
allocator for every run it hands out (8-aligned, released by its own
`run_free`), the error helpers, cancel tokens, and the leak counter:

```c
{{#include ../../examples/idl/greeter/greeter.c:runtime}}
```

The [C ABI contract](reference/abi.md#producers-in-other-languages) lists
what a hand-written producer owes its consumers, including honoring a
vtable's `GREETER_VTABLE_THREAD_AFFINE` flag once the API has callbacks.
Build it as `libgreeter.so` (`libgreeter.dylib` on macOS, `greeter.dll` on
Windows). The file defines `GREETER_BUILD` before it includes the header, so
the header's `GREETER_API` exports the symbols on Windows too:

```bash
cc -shared -fPIC -I bindings/c greeter.c -o libgreeter.so
```

Then run `weaveffi generate` for the other targets; they load it exactly as
they load a Rust producer. From Python, for example:

```bash
weaveffi generate --target python
pip install ./bindings/python
GREETER_LIBRARY=$PWD/libgreeter.so python3 -c "import greeter; print(greeter.add(2, 3))"   # 5
```

## Next steps

- [Project Configuration](guides/config.md): `[package]` metadata, `[build]`
  settings, per-target options, and how regeneration cleans up stale files.
- [Packaging](guides/packaging.md): building every platform and publishing
  the artifacts.
- [Samples and Examples](samples.md): the tested example crates, and three
  complete producers from the minimal `calculator` to the feature-complete
  `kvstore`.
- [Generators](generators/README.md): what each language gets.
- Gate CI on `weaveffi generate --check` so committed bindings can't drift;
  see [Stability and Versioning](stability.md#ci-workflow).
