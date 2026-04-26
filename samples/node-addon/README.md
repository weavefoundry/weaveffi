# Node addon sample

A Node.js **N-API** addon (via [`napi-rs`](https://napi.rs)) that loads the
`calculator` sample's `weaveffi_*` shared library at runtime and re-exports
its functions as JavaScript-friendly async methods. It's the Rust side of
the `examples/node/` end-to-end example.

## What this sample demonstrates

- **Runtime linkage** to a generated WeaveFFI C ABI cdylib via
  [`libloading`](https://docs.rs/libloading), with the library path taken
  from the `WEAVEFFI_LIB` environment variable (falling back to
  `target/debug/libcalculator.{dylib,so,dll}`).
- **`#[napi]` function exports** (`add`, `mul`, `div`, `echo`) that wrap
  the underlying `weaveffi_calculator_*` C entry points.
- **Error propagation** — the C ABI's `weaveffi_error` struct is read,
  converted to a `napi::Error` with the original message + code, and the
  buffer is freed via `weaveffi_error_clear` so nothing leaks back into
  the Node event loop.
- **Typed string ownership** — `echo` sends a `CString` into the C ABI,
  reads the returned pointer as a `CStr`, and calls
  `weaveffi_free_string` to hand the buffer back to the library's
  allocator (never the Node runtime's).
- **One-shot library handle caching** — the `Library` and resolved
  function pointers are cached in a `OnceCell` so repeated calls from JS
  do not re-dlopen the cdylib.

## IDL highlights

This sample does **not** define its own IDL; it consumes the calculator
sample's definitions from [`../calculator/calculator.yml`](../calculator/calculator.yml):

```yaml
modules:
  - name: calculator
    functions:
      - { name: add,  params: [{name: a, type: i32}, {name: b, type: i32}], return: i32 }
      - { name: mul,  params: [{name: a, type: i32}, {name: b, type: i32}], return: i32 }
      - { name: div,  params: [{name: a, type: i32}, {name: b, type: i32}], return: i32 }
      - { name: echo, params: [{name: s, type: string}],                    return: string }
```

Key IDL features exercised through this addon:

- `i32` scalar round-trips into and out of JavaScript `number`.
- `string` round-tripping that must go through the C ABI allocator
  (`weaveffi_free_string`), not `free()` or the JS garbage collector.
- `weaveffi_error` propagation from the C ABI surface into a
  JavaScript-thrown `Error`.

## Generate bindings

The C header consumed by this addon is produced by running the calculator
sample's `generate` command. From the repo root:

```bash
# Generate the C header (+ all other targets)
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated

# Or just the Node and C targets
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated --target c,node

# Build the calculator cdylib this addon loads at runtime
cargo build -p calculator

# Build this addon (produces target/debug/libindex.{dylib,so,dll})
cargo build -p weaveffi-node-addon
```

Supported `--target` values for the calculator IDL: `c`, `cpp`, `swift`,
`android`, `node`, `wasm`, `python`, `dotnet`, `dart`, `go`, `ruby`.

## What to look for in the generated output

- **`generated/c/weaveffi.h`** — the exact symbol names that this addon
  resolves at load time:
  `weaveffi_calculator_add`, `weaveffi_calculator_mul`,
  `weaveffi_calculator_div`, `weaveffi_calculator_echo`,
  `weaveffi_free_string`, and `weaveffi_error_clear`. If you rename
  functions in the IDL, the symbol lookups in this addon's `load_api()`
  will fail at runtime with a clear `libloading` error.
- **`generated/node/types.d.ts`** — the TypeScript declarations the Node
  generator emits for the same calculator IDL. This addon is an
  alternative "dynamic-load" variant of the same surface; the generated
  Node package is the "static-link via napi" variant. Both are valid ways
  to consume the calculator cdylib from JavaScript.
- **`generated/node/binding.gyp`** — the native build config the Node
  generator emits for downstream consumers; this addon instead uses
  `napi-build` from its `build.rs`.
- **`weaveffi_error` contract** — after any call, if `err.code != 0` the
  caller must (a) read `err.message` and (b) call
  `weaveffi_error_clear(&err)` to free the buffer. Look at
  `take_error(...)` in [`src/lib.rs`](src/lib.rs) for the reference
  implementation of this contract on the addon side.

## Run it

```bash
# Build the cdylib and the addon
cargo build -p calculator
cargo build -p weaveffi-node-addon

# Point WEAVEFFI_LIB at the cdylib if it isn't in target/debug/
export WEAVEFFI_LIB=target/debug/libcalculator.dylib   # macOS
# export WEAVEFFI_LIB=target/debug/libcalculator.so    # Linux
# export WEAVEFFI_LIB=target\debug\calculator.dll      # Windows

# Consume it from Node (see examples/node/ for a ready-to-run script)
```
