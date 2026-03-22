# WASM

The WASM generator produces a minimal JS loader and README to help get started
with `wasm32-unknown-unknown`. Full ergonomics are planned for future releases.

## Generated artifacts

- `generated/wasm/weaveffi_wasm.js` — ES module loader with JSDoc
- `generated/wasm/README.md` — quickstart and type conventions

## Generated code examples

### JS loader

The generated loader provides a `loadWeaveFFI` async function that
instantiates a `.wasm` module and returns its exports:

```javascript
export async function loadWeaveFFI(url) {
  const response = await fetch(url);
  const bytes = await response.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return instance.exports;
}
```

Usage:

```javascript
const wasm = await loadWeaveFFI('lib.wasm');
const sum = wasm.weaveffi_math_add(1, 2);
```

### Type conventions at the WASM boundary

WASM only supports numeric types natively (`i32`, `i64`, `f32`, `f64`).
Complex types are encoded as follows:

#### Structs

Structs are passed as **opaque handles** (`i64` pointers into linear
memory). Use the generated C ABI accessor functions to read/write fields:

```javascript
// Create a struct (returns i64 handle)
const handle = wasm.weaveffi_contacts_create();

// Read a field via accessor
const age = wasm.weaveffi_contacts_Contact_get_age(handle);

// Destroy when done
wasm.weaveffi_contacts_Contact_destroy(handle);
```

#### Enums

Enums are passed as `i32` values corresponding to the variant's integer
discriminant:

```javascript
// 0 = Red, 1 = Green, 2 = Blue
wasm.weaveffi_ui_set_color(0);
```

#### Optionals

Optional values use `0` / `null` to represent the absent case. For numeric
optionals, a separate `_is_present` flag (`i32`: 0 or 1) is used. For
handle-typed optionals, a null pointer (`0`) signals absence:

```javascript
// Present optional: (is_present=1, value=5000)
wasm.weaveffi_config_set_timeout(1, 5000);

// Absent optional: (is_present=0, value=0)
wasm.weaveffi_config_set_timeout(0, 0);
```

#### Lists

Lists are passed as a **pointer + length** pair (`i32` pointer, `i32`
length) referencing a contiguous region in linear memory. The caller is
responsible for allocating and freeing the backing memory:

```javascript
// Write data into WASM linear memory
const ptr = wasm.weaveffi_alloc(4 * items.length);
const view = new Int32Array(wasm.memory.buffer, ptr, items.length);
view.set(items);

// Pass pointer + length to the function
wasm.weaveffi_data_process(ptr, items.length);

// Free the memory
wasm.weaveffi_dealloc(ptr, 4 * items.length);
```

### Type mapping reference

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

## Build

### macOS

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

### Linux

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

The build commands are identical on both platforms since WASM is a
cross-compilation target.

Serve the `.wasm` file and load it with the provided JS helper.
