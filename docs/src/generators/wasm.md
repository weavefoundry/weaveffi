# WebAssembly

The WebAssembly target generates an npm package for a `wasm32-unknown-unknown`
build of the library: an ES module with TypeScript declarations over glue
that stages values in the module's linear memory. The JavaScript API is the
[Node.js target](node.md)'s, so the sections there on names, types, objects,
errors, callbacks, and iterators apply here unchanged; this page covers what
differs.

## What gets generated

For a library named `kvstore` (the snapshot fixtures use `kitchen_sink`):

```text
wasm/
├── package.json   ES module manifest
├── index.js       `init()`, the raw entry points, and the API
├── index.d.ts     TypeScript declarations
├── runtime.js     the shared runtime (codec, errors, wrappers)
├── linear.js      the transport: loading, staging, table functions
└── README.md
```

The package name defaults to the library identity's name; set
`package_name` under `[generators.wasm]` to override it. `weaveffi package`
adds the prebuilt module as `{library}.wasm`.

## Build the module

The glue installs async completions and callback-interface methods in the
module's function table, so link the module with an exported, growable
table:

```sh
RUSTFLAGS="-C link-arg=--export-table -C link-arg=--growable-table" \
  cargo build --release --target wasm32-unknown-unknown
```

## Load

`init()` loads the module and checks the contract before any other export
works: the module must implement ABI revision 3, and each top-level module's
checksum must match the bindings. Until the promise resolves, every other
call throws.

```js
import { init, kv } from 'kvstore';

await init();
const store = kv.Store.open('/tmp/data');
```

Without an argument, `init()` loads `{library}.wasm` from next to `index.js`
(by `fetch` in a browser, from disk on Node.js), or, on Node.js, the file
named by the `{PREFIX}_LIBRARY` environment variable. It also takes a URL, a
path, bytes, a `Response`, or a compiled `WebAssembly.Module`:

```ts
export declare function init(
  source?: string | URL | BufferSource | WebAssembly.Module | Response,
): Promise<void>;
```

`init()` may be called more than once; later calls return the same promise,
and a failed load can be retried.

## Transport

Each C symbol gets one entry point in `index.js` that checks its arguments,
copies strings and byte runs into linear memory for the call, and copies
returned strings and buffers out before releasing them:

```js
    kitchen_sink_kitchen_echo_string: (a0) => {
      let s0 = null;
      try {
        s0 = m.str(a0);
        const r = x.kitchen_sink_kitchen_echo_string(s0[0], s0[1], m.len, m.err);
        m.check();
        return m.takeStr(r, m.outLen());
      } finally {
        m.unstage(s0);
      }
    },
```

One error slot, one `out_len` slot, and one iterator item slot are allocated
at load and reused by every call. Object handles are linear-memory addresses.

Async completions and callback-interface methods are JavaScript functions
installed in the module's function table. The glue uses
`WebAssembly.Function` where the engine provides it and otherwise compiles a
tiny module that imports the function with the needed signature and
re-exports it, so no experimental flag is needed.

## Type mapping

The mapping is the Node.js target's: 64-bit integers are `bigint`, `bytes` is
`Uint8Array`, records are plain objects, rich enums are tagged unions,
C-style enums are frozen objects, interfaces are classes with `close()` and
`[Symbol.dispose]()`, optionals are `T | null`, lists are arrays, maps are
`Record<K, V>`, and `iter<T>` is a lazy `IterableIterator<T>`.

## Async and cancellation

Async functions return a `Promise`, and cancellable ones take an optional
`{ signal }` last argument, exactly as on Node.js. The module is
single-threaded and the default executor runs a future to completion inside
its launcher, so an async call has usually completed by the time the
`Promise` is returned; only a signal aborted before the call can cancel it.
A cancelled call rejects with `CancelledError` (code -5).

## Callbacks and errors

A callback runs only while a call into the module is on the stack, on the
calling thread. An implementation that throws fails the call with code -4,
as on Node.js. Because `wasm32-unknown-unknown` aborts on a panic, a producer
panic traps the module; the trap surfaces as the package's root error with
code -2, and the module may be unusable afterward.

## Emscripten

With `emscripten = true` under `[generators.wasm]`, `init` instead takes an
initialized Emscripten module (or the promise its `MODULARIZE` factory
returns), binds its underscore-prefixed exports, reads memory through
`HEAPU8`, and installs table functions with `addFunction`. Link the module
with `-sALLOW_TABLE_GROWTH`, `-sWASM_BIGINT`, and
`-sEXPORTED_RUNTIME_METHODS=addFunction,HEAPU8`, and export the library's C
symbols, including `{prefix}_alloc` and `{prefix}_dealloc`. The packaged
layout then ships glue only.

```ts
export declare function init(module: object | Promise<object>): Promise<void>;
```

## Known limitations

- The module must export its function table and allow it to grow.
- An async call can't be cancelled once launched, since it completes before
  its launcher returns.
- A producer panic traps rather than unwinding, which can leave the module's
  state inconsistent.
- Emscripten mode isn't covered by the conformance suite.
