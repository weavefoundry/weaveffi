# WASM

## Overview

The WASM target produces a typed ES module loader for
`wasm32-unknown-unknown` builds of WeaveFFI cdylibs. The loader wraps
the raw exports in idiomatic JavaScript: per-module namespaces, struct
wrapper classes with getters, thrown `Error`s instead of error slots,
`Promise`-based async functions, and automatic string/bytes staging in
linear memory. TypeScript declarations describe the whole surface.

Because a `wasm32-unknown-unknown` module is single-threaded and has no
producer thread, **callbacks and listeners are not supported** — see
[Capabilities and `allow_unsupported`](#capabilities-and-allow_unsupported).

## What gets generated

| File | Purpose |
|------|---------|
| `generated/wasm/weaveffi_wasm.js` | ES module: memory helpers, struct wrapper classes, and the async `loadWeaveffiWasm(url)` loader returning typed bindings |
| `generated/wasm/weaveffi_wasm.d.ts` | TypeScript declarations for the loader and every module namespace |
| `generated/wasm/package.json` | npm package manifest (`type: "module"`) |
| `generated/wasm/README.md` | Quickstart and boundary conventions |

## Type mapping

| IDL type     | WASM boundary | JavaScript surface |
|--------------|---------------|--------------------|
| `i32` / `u32`| `i32`         | `number`           |
| `i64`        | `i64`         | `BigInt`           |
| `f64`        | `f64`         | `number`           |
| `bool`       | `i32`         | `boolean` (0/1 at the boundary) |
| `string`     | `i32` pointer (NUL-terminated UTF-8) | `string`, staged via `weaveffi_alloc` |
| `bytes`      | `i32` pointer + `i32` length | `Uint8Array` copy |
| `handle` / `StructName` | `i32` pointer into linear memory (0 = null) | struct wrapper class with getters |
| `EnumName`   | `i32` discriminant | `number` |
| `T?`         | 0 / null pointer; scalars boxed by pointer | `T \| null` |
| `[T]`        | `i32` pointer + `i32` length | `Array` copy |
| `iter<T>`    | iterator handle + `next` out-param | drained into an `Array` |

## Example IDL → generated code

The loader exports a single async entry point that fetches,
instantiates, and wraps a `.wasm` module:

```javascript
import { loadWeaveffiWasm } from './weaveffi_wasm.js';

const api = await loadWeaveffiWasm('/your_library.wasm');
```

Functions are grouped by IDL module and have idiomatic signatures —
strings, arrays, and error handling are taken care of inside the
wrapper:

```javascript
api.events.send_message('hello');        // throws Error on failure
const all = api.events.get_messages();   // iter<string> -> string[]
```

Structs come back as wrapper classes holding the native handle, with a
getter per field and a static `create` when the struct has a
constructor:

```javascript
const result = await api.tasks.run_task('build');
console.log(result.id, result.value, result.success);
```

The raw exports stay reachable for anything not covered by the typed
surface:

```javascript
api._raw.weaveffi_alloc(16);
```

The generated `weaveffi_wasm.d.ts` mirrors all of this for TypeScript
consumers:

```typescript
export interface WeaveffiWasmModule {
  _raw: WebAssembly.Exports;
  events: {
    send_message(text: string): void;
    get_messages(): string[];
  };
}

export function loadWeaveffiWasm(url: string): Promise<WeaveffiWasmModule>;
```

## Async support

Async IDL functions return real `Promise`s. The loader grows the
module's `__indirect_function_table` and registers one JavaScript
trampoline per completion-callback signature using the
[JS Type Reflection API](https://github.com/WebAssembly/js-types)
(`new WebAssembly.Function(...)`); each call stores its
`resolve`/`reject` pair in a context map keyed by an integer id:

```javascript
run_task(name) {
  return new Promise((resolve, reject) => {
    const ctxId = _nextCtxId++;
    _asyncContexts.set(ctxId, { resolve, reject, unwrap: (w, h) => new TaskResult(w, h) });
    const [a0_p, a0_s] = _cstr(wasm, name);
    wasm.weaveffi_tasks_run_task_async(a0_p, _cbPtr_i32_i32_i32, ctxId);
    wasm.weaveffi_dealloc(a0_p, a0_s);
  });
}
```

When the producer invokes the completion callback, the trampoline looks
up the context, settles the promise, and removes the entry.

Two caveats apply:

- `WebAssembly.Function` requires a runtime with JS Type Reflection
  (recent V8/SpiderMonkey; Chrome, Firefox, Node 16+, Deno).
- The module is single-threaded: the producer must complete the
  callback on the calling thread (e.g. an executor polled by the same
  thread). A producer that spawns OS threads will not work on
  `wasm32-unknown-unknown`.

Cancellable functions expose their cancel entry point as a plain
function in the same namespace (e.g. `api.tasks.cancel_task(id)`).

## Capabilities and `allow_unsupported`

The WASM generator declares callbacks and listeners as unsupported in
its `TargetCapabilities`. If your IDL uses them, `weaveffi generate`
fails with an error listing the offending definitions rather than
silently skipping them.

To generate the rest of the surface anyway, opt in explicitly:

```toml
# weaveffi.toml
[wasm]
allow_unsupported = true
```

or inline in the IDL:

```yaml
generators:
  wasm:
    allow_unsupported: true
```

With the opt-in, unsupported entry points are generated as **explicit
throwing stubs** — calling `register_message_listener` throws an
`Error` explaining that listeners need a native target — so the gap is
visible at the call site instead of failing silently.

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
  import { loadWeaveffiWasm } from './weaveffi_wasm.js';
  const api = await loadWeaveffiWasm('/your_library.wasm');
</script>
```

## Memory and ownership

- The wrapper stages strings, bytes, and arrays into linear memory with
  the exported `weaveffi_alloc` / `weaveffi_dealloc` and releases them
  after the call — you do not manage buffers for typed calls.
- Producer-owned returns (strings, arrays, struct fields) are copied to
  JavaScript values and freed via `weaveffi_free_string` /
  `weaveffi_dealloc` inside the wrapper.
- Struct wrapper objects hold a native handle. JavaScript has no
  deterministic destructors; the underlying allocation lives until the
  module is dropped. Treat handles as owned by the module instance.
- Error slots are allocated, checked, and cleared internally; failures
  surface as thrown `Error`s with the producer's code and message.
- When you bypass the typed surface via `_raw`, the conventions at the
  top of `weaveffi_wasm.js` apply and every alloc must be paired with a
  dealloc.

## Troubleshooting

- **`WebAssembly.Function is not a constructor`** — the runtime lacks
  JS Type Reflection. Use a current Chrome/Firefox/Node/Deno, or avoid
  async IDL functions for this target.
- **`LinkError: import object field 'env' is not a Function`** — the
  loader instantiates with an empty imports object. If your Rust crate
  imports host functions, extend `loadWeaveffiWasm` to pass them in.
- **An async call never settles** — the producer must invoke the
  completion callback on the same thread; `std::thread::spawn` does not
  exist on `wasm32-unknown-unknown`.
- **Out-of-memory after many `_raw` calls** — every pointer returned
  from the module must be deallocated; the typed wrappers do this for
  you, raw calls do not.
- **The `.wasm` file fails to instantiate** — the build artifact must
  be `wasm32-unknown-unknown`. `wasm32-wasi` modules require WASI
  imports and cannot run in the browser without a polyfill.
