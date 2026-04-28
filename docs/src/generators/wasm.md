# WASM

## Overview

The WASM target produces a minimal ES module loader plus a README to
help instantiate `wasm32-unknown-unknown` builds of WeaveFFI cdylibs.
Higher-level ergonomics (struct wrappers, async helpers) are intentionally
kept out of the generator and live in your application code so that the
WASM module stays as small as possible.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/wasm/weaveffi_wasm.js` | ES module loader with JSDoc |
| `generated/wasm/README.md` | Quickstart and type conventions |

## Type mapping

| IDL type     | WASM type | Convention                           |
|--------------|-----------|--------------------------------------|
| `i32`        | `i32`     | Direct value                         |
| `u32`        | `i32`     | Direct value (unsigned interpretation)|
| `i64`        | `i64`     | Direct value                         |
| `f64`        | `f64`     | Direct value                         |
| `bool`       | `i32`     | 0 = false, 1 = true                  |
| `string`     | `i32+i32` | Pointer + length in linear memory    |
| `bytes`      | `i32+i32` | Pointer + length in linear memory    |
| `handle`     | `i64`     | Opaque 64-bit identifier             |
| `StructName` | `i64`     | Opaque handle (pointer)              |
| `EnumName`   | `i32`     | Integer discriminant                 |
| `T?`         | varies    | `_is_present` flag or null pointer   |
| `[T]`        | `i32+i32` | Pointer + length in linear memory    |

## Example IDL → generated code

The loader exports a single async function that fetches and instantiates
a `.wasm` module:

```javascript
export async function loadWeaveFFI(url) {
  const response = await fetch(url);
  const bytes = await response.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return instance.exports;
}
```

Use it directly:

```javascript
const wasm = await loadWeaveFFI('lib.wasm');
const sum = wasm.weaveffi_math_add(1, 2);
```

Structs are passed across the boundary as opaque `i64` handles:

```javascript
const handle = wasm.weaveffi_contacts_create();
const age = wasm.weaveffi_contacts_Contact_get_age(handle);
wasm.weaveffi_contacts_Contact_destroy(handle);
```

Lists and strings cross via pointer+length pairs in linear memory:

```javascript
const ptr = wasm.weaveffi_alloc(4 * items.length);
const view = new Int32Array(wasm.memory.buffer, ptr, items.length);
view.set(items);
wasm.weaveffi_data_process(ptr, items.length);
wasm.weaveffi_dealloc(ptr, 4 * items.length);
```

## Build instructions

macOS / Linux / Windows (cross-compilation, all hosts):

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release -p your_library
```

The resulting `.wasm` is in `target/wasm32-unknown-unknown/release/`.
Serve it over HTTP and load it with the generated helper:

```html
<script type="module">
  import { loadWeaveFFI } from './weaveffi_wasm.js';
  const wasm = await loadWeaveFFI('/your_library.wasm');
</script>
```

## Memory and ownership

- WASM linear memory is owned by the module. Use the exported
  `weaveffi_alloc` / `weaveffi_dealloc` (or `__wbindgen_*` helpers if
  you bundle `wasm-bindgen`) to manage buffers passed to the WASM
  module — every alloc must be paired with a dealloc.
- Strings and byte buffers crossing into the WASM module require
  copying their contents into linear memory before the call.
- Struct handles must be paired with their generated `_destroy`
  function to free the Rust-side allocation.
- The host JS side is responsible for keeping references alive while
  the WASM call runs; the WASM module cannot reach back into JS-owned
  memory.

## Async support

Native async is **not yet emitted** by the WASM target. Async IDL
functions still produce their synchronous C ABI counterparts and a
JavaScript shim that wraps the call in a `Promise.resolve(...)`. For
true async (e.g. `Promise`-returning JS functions backed by Rust
futures) bundle the cdylib with `wasm-bindgen` or `wasm-pack` and
expose the resulting `.wasm` to the loader.

## Troubleshooting

- **`LinkError: import object field 'env' is not a Function`** — the
  loader instantiates with an empty imports object. If your Rust crate
  imports host functions, extend `loadWeaveFFI` to pass them in.
- **Out-of-memory after many calls** — every pointer returned from the
  WASM module must be deallocated. Wrap calls in helper functions that
  always dealloc on `finally`.
- **Wrong endianness or struct layout** — WASM is little-endian and
  uses `wasm32` pointers. Always read with the matching `TypedArray`
  view (`Int32Array`, `Uint8Array`, ...).
- **The `.wasm` file fails to instantiate** — the build artifact must
  be `wasm32-unknown-unknown`. `wasm32-wasi` modules require WASI
  imports and cannot run in the browser without a polyfill.
