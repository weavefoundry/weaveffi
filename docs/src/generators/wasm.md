# WebAssembly

The WebAssembly target generates an npm package for a `wasm32-unknown-unknown`
build of the library: an ES module with TypeScript declarations over a
transport that stages values in the module's linear memory and calls its
[C ABI](../reference/abi.md) (revision 6). The JavaScript API is the
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
├── index.js       the C signatures, `init()`, and the API
├── index.d.ts     TypeScript declarations
├── runtime.js     the shared runtime (codec, checks, errors, wrappers)
├── debug.js       the `./debug` export: leak counters for tests
├── debug.d.ts
├── linear.js      the transport: loading, staging, table functions
└── README.md
```

The package name defaults to the package identity's `name`; set `name` under
`[generators.wasm]` to override it. The package has no dependencies.
`weaveffi package` writes the npm tarball `wasm/{package}-{version}.tgz` with
the module `weaveffi build` linked for `wasm32` as `{library}.wasm`.

## Build the module

The transport installs async completions and callback-interface methods in
the module's function table, so link the module with an exported, growable
table:

```sh
RUSTFLAGS="-C link-arg=--export-table -C link-arg=--growable-table" \
  cargo build --release --target wasm32-unknown-unknown
```

`weaveffi build --platforms wasm32` passes those flags itself and writes the
module to `target/weaveffi/wasm32/{library}.wasm`.

## Load

`init()` loads the module and checks it before any other export works: the
module must implement ABI revision 6 and export every declaration's contract
function with the hash the bindings were generated with (see the
[Node.js page](node.md#load-time-checks)). A failed check rejects with an
`Error` naming the declaration (`kvstore: Store.put changed since these
bindings were generated`). Until the promise resolves, every other call
throws.

```js
import { init, Store } from 'kvstore';

await init();
const store = Store.open('/tmp/data');
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

`linear.js` binds the same C signatures (`$sigs`) the Node.js transport
does, so the API above it is identical. Each bound function lowers its
arguments to the module's C calling convention for `wasm32`, which Rust's
`extern "C"` follows: integers of 32 bits or fewer, `bool`, `size_t`, and
pointers are `i32`; `int64_t` and `uint64_t` are `i64`; `float` and
`double` are `f32` and `f64`. A struct (a `{prefix}_run`, a
`{prefix}_opt_*`, or a callback-interface argument) is passed indirectly, as
the `i32` address of a copy the caller owns, and a struct return comes back
through a hidden first `i32` parameter, the address the callee writes it to,
with the function returning nothing. None of the ABI's structs is a single
scalar, so none is passed directly. These are the export types a build of
the samples has:

```text
kvstore_Store_tick         (i32 self, i64 seconds, i32 out_err) -> i64
kvstore_Store_open         (i32 path*, i32 out_err) -> i32
kvstore_Store_put          (i32 ret*, i32 self, i32 key*, i32 value*, i32 kind, i32 ttl_seconds*, i32 out_err) -> ()
kvstore_Store_expires_at   (i32 ret*, i32 self, i32 key*, i32 out_err) -> ()
kvstore_Store_subscribe    (i32 self, i32 listener*, i32 out_err) -> i32
kvstore_run_free           (i32 run*) -> ()
codec_echo_opt_bool        (i32 ret*, i32 value*, i32 out_err) -> ()
```

The copies live on a small stack the transport carves from linear memory,
popped as each call returns. A run argument is staged in a run from
`{prefix}_alloc` (8-aligned, so a typed array's elements are aligned for
their type) and released with `{prefix}_run_free` once the call returns; a
returned run is copied out and released the same way. The transport reads
and writes multi-byte values through a `DataView`. One error slot and one
iterator item slot are reused by every call; they, the stack, and the
callback vtables live in 8-byte-aligned blocks carved from arenas the
transport allocates once and never frees. The `./debug` export's
`debugLive(4)` leaves those arenas out of its count of runs. Object handles
are linear-memory addresses.

Async completions and callback-interface methods are JavaScript functions
installed in the module's function table, with the same lowering in
reverse (a method returning a run or an optional scalar writes it through
its hidden first parameter). The transport uses `WebAssembly.Function` where
the engine provides it and otherwise compiles a tiny module that imports the
function with the needed signature and re-exports it, so no experimental
flag is needed.

## Type mapping

The mapping is the Node.js target's: 64-bit integers are `bigint`, `bytes` is
`Uint8Array`, records are plain objects, rich enums are tagged unions,
C-style enums are frozen objects, interfaces are classes with `close()` and
`[Symbol.dispose]()`, optionals are `T | null`, lists are arrays (a numeric
list argument may also be the matching typed array), maps are `Record`
objects (a `Map` is accepted as an argument), and `iter<T>` is a lazy
`NativeIterator<T>` with `close()`. Integer arguments are range-checked the
same way, so a value that throws a `RangeError` on one target throws it on
the other. A `usize` is 32 bits on `wasm32`, so a wider value fails the
producer's conversion with code -3.

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

Callback interfaces work as on Node.js: returns of every family (a string,
bytes, buffer, or typed-array return is copied into a run from
`{prefix}_alloc` that the producer adopts, an optional scalar is a
`{prefix}_opt_*` struct, and an object return is a new reference), errors
reported by each method's `throws` (a domain's codes with their fields, -1
for `throws: any`, -4 for a method that doesn't throw), and `null` for an
optional callback (`Cb?`), which passes a null vtable. The vtable starts
with its `size`, `flags` (0), and `free` entries, like any consumer's.

There are no threads, so a callback runs only while a call into the module
is on the stack, on the calling thread, and the Node.js target's threading
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
- Calls nested more than a few hundred deep (callbacks that call back into
  the module) exhaust the transport's 16 KiB stack of struct copies and
  throw a `RangeError`.
- Only `wasm32-unknown-unknown` builds are supported.
