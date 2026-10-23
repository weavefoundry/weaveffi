# WebAssembly

The WebAssembly target generates an npm package for a `wasm32-unknown-unknown`
build of the library: an ES module with TypeScript declarations over glue
that stages values in the module's linear memory. The JavaScript API is the
[Node.js target](node.md)'s, so the sections there on names, types, objects,
errors, callbacks, and iterators apply here unchanged; this page covers what
differs.

WebAssembly is a [Tier 2](../stability.md#target-tiers) target: it may lag
behind a new ABI revision, but it passes the same conformance suite before a
release.

## What gets generated

For a library named `kvstore` (the snapshot fixtures use `kitchen_sink`):

```text
wasm/
├── package.json   ES module manifest
├── index.js       `init()`, the raw entry points, and the API
├── index.d.ts     TypeScript declarations
├── runtime.js     the shared runtime (codec, checks, errors, wrappers)
├── debug.js       the `./debug` export: leak counters for tests
├── debug.d.ts
├── linear.js      the transport: loading, staging, table functions
└── README.md
```

The package name defaults to the package identity's `name`; set `name` under
`[generators.wasm]` to override it. `weaveffi package` writes the npm tarball
`wasm/{package}-{version}.tgz` with the module `weaveffi build` linked for
`wasm32` as `{library}.wasm`.

## Build the module

The glue installs async completions and callback-interface methods in the
module's function table, so link the module with an exported, growable
table:

```sh
RUSTFLAGS="-C link-arg=--export-table -C link-arg=--growable-table" \
  cargo build --release --target wasm32-unknown-unknown
```

`weaveffi build --platforms wasm32` passes those flags itself and writes the
module to `target/weaveffi/wasm32/{library}.wasm`.

## Load

`init()` loads the module and checks the contract before any other export
works: the module must implement ABI revision 5, and each top-level module's
contract table must hold every row the bindings were generated with (see
the [Node.js page](node.md#load-time-checks)). A failed check rejects
with an `Error` naming the declaration (`kvstore: kv.Store.put changed
since these bindings were generated`). Until the promise resolves, every
other call throws.

```js
import { init, kv } from 'kvstore';

await init();
const store = kv.Store.open('/tmp/data');
```

Without an argument, `init()` reads the file named by the `{PREFIX}_LIBRARY`
environment variable (`KVSTORE_LIBRARY`) when it's set on Node.js, and
otherwise loads `{library}.wasm` from next to `index.js` (by `fetch` in a
browser, from disk on Node.js). It also takes a URL, a path (resolved
against `index.js`), bytes, a `Response`, or a compiled
`WebAssembly.Module`:

```ts
export declare function init(
  source?: string | URL | BufferSource | WebAssembly.Module | Response,
): Promise<void>;
```

`init()` may be called more than once; later calls return the same promise,
and a failed load can be retried.

## Transport

Each C symbol gets one entry point in `index.js` that checks its arguments
(range-checking integers), splits an optional scalar into its flag and
value, copies strings, byte runs, and typed arrays into linear memory for
the call, and copies returned strings, buffers, and typed arrays out before
releasing them:

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

Staged arguments are runs from `{prefix}_alloc` (8-aligned, so a typed
array's elements are aligned for their type), released with
`{prefix}_free_bytes` once the call returns. The glue reads and writes
multi-byte values through a `DataView`. One error slot and one each of the
`out_len`, `out_value`, and iterator item slots are reused by every call;
they and the callback vtables live in 8-byte-aligned blocks carved from
arenas the glue allocates once and never frees. The `./debug` export's
`debugLive(4)` leaves those arenas out of its count of byte runs. Object
handles are linear-memory addresses.

Async completions and callback-interface methods are JavaScript functions
installed in the module's function table. The glue uses
`WebAssembly.Function` where the engine provides it and otherwise compiles a
tiny module that imports the function with the needed signature and
re-exports it, so no experimental flag is needed.

## Type mapping

The mapping is the Node.js target's: 64-bit integers are `bigint`, `bytes` is
`Uint8Array`, records are plain objects, rich enums are tagged unions,
C-style enums are frozen objects, interfaces are classes with `close()` and
`[Symbol.dispose]()`, optionals are `T | null`, lists are arrays (a numeric
list argument may also be the matching typed array), maps are `Record`
objects (a `Map` is accepted as an argument), and `iter<T>` is a lazy
`NativeIterator<T>` with `close()`. Integer arguments are range-checked the
same way, so a value that throws a `RangeError` on one target throws it on
the other.

## Async and cancellation

Async functions return a `Promise`, and cancellable ones take an optional
`{ signal }` last argument, exactly as on Node.js. The module is
single-threaded, and on `wasm32` the default executor polls a future inline
until it completes, inside its launcher, so an async call has completed by
the time the `Promise` is returned; only a signal aborted before the call
can cancel it. A future that's still pending once nothing can wake it (it
awaits something only another thread or a reactor would complete) fails
with code -1 instead (`async function suspended with no executor on
wasm32`). A cancelled call rejects with `CancelledError` (code -5).

## Callbacks and errors

Callback interfaces work as on Node.js: returns of every family (a
string, bytes, buffer, or typed-array return is copied into a run from
`{prefix}_alloc` that the producer adopts, an optional scalar is the
method's `bool` result plus the value behind its `out_value` slot, and an
object return is a new reference), errors reported by each method's
`throws` (a domain's codes with their fields, -1 for `throws: any`, -4 for
a method that doesn't throw), and `null` for an optional callback (`Cb?`),
which passes a null vtable. The vtable starts with its `size`, `flags` (0),
and `free` entries, like any consumer's.

There are no threads, so a callback runs only while a call into the module
is on the stack, on the calling thread, and the Node.js addon's threading
rules don't arise.

## Traps

Because `wasm32-unknown-unknown` aborts on a panic, a producer panic traps
the module. The call that trapped fails with the package's root error, code
-2 (`the native library trapped: unreachable`). A trap can leave the
module's memory inconsistent, so it also poisons the instance: every later
call fails with code -2 (`kvstore: the WebAssembly instance trapped earlier
(...) and can't be used anymore`) without running, while releases (closing
a wrapper or an iterator, a garbage-collected wrapper, a cancel token) do
nothing, so no finalizer throws. Load the module again in a new process or
page to recover.

## Known limitations

- The module must export its function table and allow it to grow.
- An async call can't be cancelled once launched, since it completes before
  its launcher returns, and a future that waits on another thread or an
  external event fails with code -1.
- A producer panic traps rather than unwinding, and the instance is
  unusable afterward (see [Traps](#traps)).
- Only `wasm32-unknown-unknown` builds are supported.
