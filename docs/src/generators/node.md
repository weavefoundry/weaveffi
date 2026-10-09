# Node.js

The Node.js target generates an npm package: an ES module with TypeScript
declarations, and a small N-API addon that calls the library's
[C ABI](../reference/abi.md) (revision 5). The JavaScript API is shared with
the [WebAssembly target](wasm.md); only the transport underneath differs, so
code written against one runs against the other.

Node.js is a [Tier 1](../stability.md#target-tiers) target: it tracks every
ABI revision as it lands and runs the full conformance suite in CI.

## What gets generated

For a library named `kvstore` (the snapshot fixtures use `kitchen_sink`):

```text
node/
├── package.json      ES module manifest; `npm install` compiles the addon only without a prebuilt one
├── binding.gyp       builds the addon and links the library
├── kvstore_node.c    the N-API addon
├── kvstore.h         a copy of the C header the addon compiles against
├── index.js          the API: one namespace export per top-level module
├── index.d.ts        TypeScript declarations
├── runtime.js        the shared runtime (codec, checks, errors, wrappers, load-time checks)
├── debug.js          the `./debug` export: leak counters for tests
├── debug.d.ts
└── README.md
```

The package name defaults to the package identity's `name`; set `name` under
`[generators.node]` in `weaveffi.toml` to override it, and `node_engine` to
change `engines.node` (default `>=18`). The addon is named
`{library}_node.node`.

## Install, build, and load

The generated directory is a complete package. Its loader uses a prebuilt
addon when one matches the platform (from an installed
`{package}-{os}-{cpu}` package, or under `prebuilds/<os>-<cpu>/`), and its
`install` script compiles the addon with node-gyp only when none does. To
compile it, install the package pointing the build at the native library
with the `{PREFIX}_LIBRARY` environment variable (the library file or its
directory) or the matching npm option:

```sh
KVSTORE_LIBRARY=/opt/kvstore/lib/libkvstore.so npm install ./node
npm install ./node --kvstore-library=/opt/kvstore/lib
```

Without either, node-gyp looks for the library in the package directory.
The addon links `{library}` and carries an rpath to its own directory and to
the directory it was linked against, so a library copied next to the addon
also loads. The setting applies when the addon is built; at run time the
operating system's loader finds the library as usual.

```js
import { kv, KvstoreError } from 'kvstore';

const store = kv.Store.open('/tmp/data');
store.put('greeting', new TextEncoder().encode('hi'), kv.EntryKind.Volatile, null);
store.close();
```

`weaveffi package` writes npm tarballs in the esbuild-style layout: one
`{package}-{os}-{cpu}` package per desktop platform, gated by npm `os` and
`cpu` and carrying the library and the addon `weaveffi build` prebuilt for
it, plus the main package, which lists them in `optionalDependencies`. So
`npm install` needs no compiler on a platform whose addon `weaveffi build`
prebuilt; elsewhere, Windows included (where the addon is never prebuilt),
it compiles the addon against the installed platform package's library.
See [Packaging](../guides/packaging.md) for which addons are prebuilt.

## Load-time checks

Importing the package loads the addon and, before any other call, checks
that the library matches the bindings (see
[Load-time checks](../reference/abi.md#load-time-checks)). The library must
implement ABI revision 5, and every top-level module's contract table must
hold each row the bindings were generated with: one per declaration, error
code, and callback method. `index.js` embeds those rows, each with its
canonical signature:

```js
const $contract = [
  ['kitchen_sink_kitchen_contract', [
    [0x01d4f09bbddc986fn, 0xffcf2bd3b04aa002n, 'kitchen.Gadget.new'], // constructor new(i64) -> Gadget
    // ...
  ]],
];
$verify($raw, 'kitchen_sink', 'kitchen_sink', 5, $contract);
```

If a check fails, the import throws an `Error` naming the declaration:
`kvstore: kv.Store.put is missing from the library` or `kvstore:
kv.Store.put changed since these bindings were generated`. Rows the library
has and the bindings don't are fine, so a library that only adds to its API
(declarations, error codes, callback methods) keeps working with older
bindings. The import fails the same way when the addon can't be found or
the library can't be loaded, so `await import(...)` in a `try` can fall
back.

## Modules and names

Each top-level IDL module is a frozen namespace object, and nested modules
are nested namespaces, so equal names in different modules never collide:

```js
export const kitchen = Object.freeze({
  KitchenError: kitchen$KitchenError,
  NotFoundError: kitchen$NotFoundError,
  InvalidInputError: kitchen$InvalidInputError,
  PantryError: kitchen$PantryError,
  OutOfStockError: kitchen$OutOfStockError,
  SpoiledError: kitchen$SpoiledError,
  Priority: kitchen$Priority,
  Gadget: kitchen$Gadget,
  boolId: kitchen$boolId,
```

Functions, methods, and parameters are camelCase. A free function,
parameter, or module whose name is a JavaScript reserved word gains a
trailing underscore (a function `delete` becomes `delete_`). Methods keep
reserved words, which are legal property names (`store.delete(key)`),
except that an instance method named `close` or `constructor`, and a
static named `name`, `length`, `prototype`, or `caller`, gains one too
([reserved member names](../reference/naming.md#identifiers-in-generated-code)).
Record fields keep their IDL spelling. Doc comments in `index.d.ts` name
declarations in this spelling too: an IDL doc or deprecation message that
says `` `new_op` `` reads `` `newOp` `` there.

## Type mapping

| IDL type | TypeScript type | Notes |
|----------|-----------------|-------|
| `i8`, `i16`, `i32`, `u8`, `u16`, `u32` | `number` | A fractional, non-finite, or out-of-range argument throws a `RangeError`; nothing wraps |
| `i64`, `u64` | `bigint` | Integral numbers in range are accepted as arguments; anything else throws a `RangeError` |
| `f32`, `f64` | `number` | NaN, the infinities, and `-0` round-trip; an `f32` argument rounds as `Float32Array` does |
| `bool` | `boolean` | |
| `string` | `string` | UTF-8, passed as pointer and length, so interior NULs survive |
| `bytes` | `Uint8Array` | |
| record | `interface` | A plain object; fields keep their IDL names |
| C-style enum | `enum` | A frozen object with forward and reverse mappings (negative values too) |
| rich enum | tagged union | `{ tag: 'Circle', radius: 2.5 }` |
| interface | `class` | See [Objects](#objects-and-lifetime) |
| callback interface | `interface` | Any object with the methods; see [Callback interfaces](#callback-interfaces) |
| `T?` | `T \| null` | `undefined` is accepted as `null`. An optional scalar or C-style enum crosses directly as a flag and a value, with no buffer |
| `[T]` | `T[]` | A list of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`, or `f64` crosses as a typed array: an argument may be an array (checked element by element) or the matching typed array (`Float64Array` for `[f64]`, passed as is), and a result is a plain array |
| `{K: V}` | `Record<string, V>` | `Record<number, V>` for integer keys of 32 bits or fewer, `Partial<Record<E, V>>` for C-style enum keys; keys are property names (strings) at run time, and a `Map` is accepted as an argument |
| `iter<T>` | `NativeIterator<T>` | Lazy; an `IterableIterator<T>` with `close()` and `[Symbol.dispose]()` |

Arguments are checked before the native call: a value of the wrong type
throws a `TypeError`, and an integer that is fractional or out of its type's
range a `RangeError`, whether it's an argument, an element of a list, a
field of a record, or a callback's return value. The addon checks integer
arguments on the `double` it receives before converting it, so the result
is the same on every platform.

Value buffers are encoded and decoded by one function per type: each record
and rich enum (`$w$kv$Entry`, `$r$kv$Entry`), each interface carried as an
object token, and each distinct optional, list, and map type in the API,
named by its canonical stem (`$w_list_opt_Entry` writes `[Entry?]`). Call
sites name these functions.

## Objects and lifetime

An interface is a class whose instances each own one strong reference to the
native object. `close()` releases it, and so does `[Symbol.dispose]()` (for
`using` declarations); both are idempotent, and a wrapper that is garbage
collected unclosed is released by a `FinalizationRegistry`. Every call lends
the wrapper to the native side for its duration, so a `close()` that races
the call (from a callback, or while an async method is pending) waits for it
to return. Using a closed wrapper throws a code -3 error.

```js
  poke(times) {
    const $self = $lend(this, kitchen$Gadget);
    try {
      return $raw.kitchen_sink_kitchen_Gadget_poke($self, times);
    } catch ($e) {
      throw $from$kitchen$KitchenErrors($e);
    } finally {
      $unlend(this);
    }
  }
```

The synchronous constructor named `new` is the class constructor; other
constructors are static factories, and a class without one can't be
constructed directly. Objects returned, awaited, iterated, decoded from a
record, or passed to a callback method are new wrappers the receiver owns.
Two wrappers may share one native object.

## Errors

Every error extends the package's root class, `{Package}Error` (here
`KitchenSinkError`), which carries the ABI `code`:

- Each error domain is a class, named by the shared rule (a domain
  `KitchenErrors` is `kitchen.KitchenError`, `KvError` stays `KvError`, and
  `Failure` becomes `FailureError`), and each of its codes a subclass
  (`kitchen.NotFoundError`) with a static `CODE` and the IDL message as its
  default message. A module may declare several domains, and codes of
  different domains may share a value. Payload fields become properties. A
  code without fields is constructed as `new kv.InvalidPathError(message?)`;
  a code with fields takes them first:
  `new kv.RejectedError({ key, reason }, message?)`.
- A function declared `throws: SomeDomain` fails with that domain's classes.
  Domains are open: a positive code the bindings don't know (the library
  added it later) is an instance of the domain class itself, with its
  `code` and message.
- A function declared `throws: any` fails with the root class, code -1, and
  the library's message.
- A cancelled call rejects with `CancelledError` (code -5).
- Other runtime failures throw the root class itself: -2 producer panic, -3
  marshalling failure, -4 a callback implementation failed.

A failure of a function that doesn't declare `throws` is a producer bug, and
the binding throws the root class with the code and the producer's message
(see [the trap policy](../guides/errors-and-memory.md#the-trap-policy)).

## Async and cancellation

An async function returns a `Promise`. The completion may arrive on any
producer thread; the addon settles the promise on the JavaScript thread. A
cancellable function takes an optional last argument with an `AbortSignal`:

```ts
  export function doCancellable(input: string, options?: { signal?: AbortSignal }): Promise<string>;
```

Aborting the signal cancels the native token, and the call rejects with
`CancelledError`. An already-aborted signal cancels the call before it
starts.

## Callback interfaces

Any object with the interface's methods implements it; the methods are
camelCase. Arguments arrive converted: strings and bytes as `string` and
`Uint8Array`, records decoded, and objects as new wrappers the
implementation owns (close them, or let the garbage collector release
them). A parameter of an optional callback interface (`Cb?`) takes `null`
for none.

A method's return value is checked and handed to the library in its family's
form:

| IDL return | Return from JavaScript | What the library receives |
|------------|------------------------|---------------------------|
| direct (numbers, `bool`, C-style enums) | the value | the value (integers range-checked) |
| optional scalar (`i32?`, `Mode?`, ...) | the value, or `null` | a flag and the value |
| numeric list (`[f64]`, ...) | an array or the matching typed array | a copy in a run the library adopts |
| `string`, `bytes` | a `string`, a `Uint8Array` | a copy in a run the library adopts |
| record, rich enum, other `T?`, `[T]`, `{K:V}` | the value | its encoding, in a run the library adopts |
| `I` | a wrapper (never `null`) | a new strong reference to its object |
| `I?` | a wrapper or `null` | a new strong reference, or null |

Arguments arrive the same way the API returns values: optional scalars as
the value or `null`, numeric lists as plain arrays. The implementation keeps
its own wrapper; the library gets a reference of its own. Nothing unwinds
through native code; how a failure reaches the library follows the method's
`throws`:

- A method declared `throws: SomeDomain` may throw one of that domain's
  errors, and the library receives its code and fields. (A Rust producer
  re-renders the message from the fields.) Any other exception is reported
  as code -1 with its message.
- A method declared `throws: any` reports any exception as code -1 with its
  message.
- A method that doesn't declare `throws` reports an exception, or a return
  value of the wrong type, as code -4 with its message.

```js
const policy = {
  ttlFor(key, requested) {
    return key.startsWith('tmp/') ? 60n : requested;
  },
  admit(entry) {
    if (entry.key.startsWith('secret')) {
      throw new kv.RejectedError({ key: entry.key, reason: 'no secrets' });
    }
    return { ...entry, tags: ['admitted'] };
  },
  route(key, home) {
    return home;
  },
};
store.setPolicy(policy);
```

### Threading

The producer may call back from any thread, and every JavaScript call runs
on the JavaScript thread (the main thread or the worker that passed the
implementation):

- A call made on the JavaScript thread (because the JavaScript thread is
  inside a call into the library) runs directly, nested inside that call.
- A call made on any other thread is queued for the JavaScript thread and
  runs from its event loop, while the producer thread waits for the result.
  Nothing runs while the JavaScript thread is busy, so such calls are
  delivered only once the current JavaScript task returns to the event loop.
- The library's release of an implementation (its vtable's `free`) may come
  from any thread; the addon drops the implementation on the JavaScript
  thread.

The waiting in the second rule could deadlock: if the JavaScript thread is
inside a synchronous call into the library that itself waits for the thread
making the callback (joins it, or waits on a lock it holds), neither thread
can proceed. The addon detects this instead of hanging. It counts the
synchronous calls in progress on each JavaScript thread, and a callback from
another thread that has waited while the JavaScript thread stayed inside one
synchronous call for a second gives up: the callback fails with code -4 and
a message saying that it would deadlock, the producer sees that failure, and
the synchronous call can finish. A callback that waits through a shorter
synchronous call, or for the event loop, simply runs later. So:

- Make calls that wait for callbacks from other threads `async`, or have
  the producer call back on the calling thread.
- Avoid synchronous calls that block for a second or more while other
  threads call back; such a callback fails with -4 rather than waiting
  longer.
- Return to the event loop regularly while callbacks from other threads are
  expected: a long-running JavaScript task delays them.

## Iterators

An `iter<T>` result is a lazy `NativeIterator<T>`: each `next()` makes one
native call. The native iterator is destroyed when it's exhausted, when it
fails, on `return()` (which `for...of` calls on `break`), on `close()` or a
`using` declaration, or when the iterator is garbage collected.

```ts
using chunks = kitchen.streamChunks();
const first = chunks.next(); // released when the block exits
```

## Leak counters

The package's `./debug` export reads the library's live-resource counters,
for tests of code that uses the bindings. It isn't part of the API:

```js
import { debugLive } from 'kvstore/debug';

debugLive(0); // live objects, as a bigint (1 callbacks, 2 iterators, 3 cancel tokens, 4 byte runs)
```

Kind -1 is `1n` when the library counts at all (built with `weaveffi`'s
`leak-check` feature); otherwise every counter is `0n`.

## Known limitations

- A callback from another thread fails with -4 if it waits for a second on
  a synchronous call (see [Threading](#threading)).
- A callback implementation that holds a wrapper of the object retaining it
  forms a cycle the garbage collector can't see; close the wrapper or
  release the callback explicitly.
- `{PREFIX}_LIBRARY` selects the library at build time, not at run time.
- Windows builds link `{library}.lib`, and the DLL must be on the loader's
  search path.
- A numeric list result is copied into a plain array. As an argument, the
  matching typed array is lent to the library without a copy, while a plain
  array is checked and copied first.
- The package is ESM only, and its declarations need TypeScript 5.2 or
  later (for `Symbol.dispose`).
