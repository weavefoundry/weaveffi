# Node.js

The Node.js target generates an npm package: an ES module with TypeScript
declarations, and a small N-API addon that calls the library's C ABI. The
JavaScript API is shared with the [WebAssembly target](wasm.md); only the
transport underneath differs, so code written against one runs against the
other.

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
├── runtime.js        the shared runtime (codec, errors, wrappers, load-time checks)
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
implement ABI revision 4, and every top-level module's contract table must
hold each declaration the bindings were generated with. `index.js` embeds
those entries:

```js
const $contract = [
  ['kv', [
    [0x0969575bfbb012d7n, 0xebd38766e3532c4fn, 'kv.Store.fork'],
    // ...
  ]],
];
$verify($raw, 'kvstore', 'kvstore', 4, $contract);
```

If a check fails, the import throws an `Error` naming the declaration:
`kvstore: kv.Store.put is missing from the library` or `kvstore:
kv.Store.put changed since these bindings were generated`. Declarations the
library has and the bindings don't are fine, so a library that only adds to
its API keeps working with older bindings.

## Modules and names

Each top-level IDL module is a frozen namespace object, and nested modules
are nested namespaces, so equal names in different modules never collide:

```js
export const kitchen = Object.freeze({
  KitchenErrorsError: kitchen$KitchenErrorsError,
  NotFoundError: kitchen$NotFoundError,
  InvalidInputError: kitchen$InvalidInputError,
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
Record fields keep their IDL spelling.

## Type mapping

| IDL type | TypeScript type | Notes |
|----------|-----------------|-------|
| `i8`, `i16`, `i32`, `u8`, `u16`, `u32` | `number` | |
| `i64`, `u64` | `bigint` | Integral numbers are accepted as arguments; out-of-range values throw a `RangeError` |
| `f32`, `f64` | `number` | NaN, the infinities, and `-0` round-trip |
| `bool` | `boolean` | |
| `string` | `string` | UTF-8, passed as pointer and length, so interior NULs survive |
| `bytes` | `Uint8Array` | |
| record | `interface` | A plain object; fields keep their IDL names |
| C-style enum | `enum` | A frozen object with forward and reverse mappings (negative values too) |
| rich enum | tagged union | `{ tag: 'Circle', radius: 2.5 }` |
| interface | `class` | See [Objects](#objects-and-lifetime) |
| callback interface | `interface` | Any object with the methods; see [Callback interfaces](#callback-interfaces) |
| `T?` | `T \| null` | `undefined` is accepted as `null` |
| `[T]` | `T[]` | |
| `{K: V}` | `Record<string, V>` | `Record<number, V>` for integer keys of 32 bits or fewer, `Partial<Record<E, V>>` for C-style enum keys; keys are property names (strings) at run time, and a `Map` is accepted as an argument |
| `iter<T>` | `IterableIterator<T>` | Lazy |

Arguments are type-checked: a value of the wrong type throws a `TypeError`
before the native call.

Value buffers are encoded and decoded by one function per type: each record
and rich enum (`$w$kv$Entry`, `$r$kv$Entry`), each interface carried as an
object token, and each distinct optional, list, and map type in the API
(`$w_list_opt_Entry` writes `[Entry?]`). Call sites name these functions.

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

- Each error domain is a class (`kitchen.KitchenErrorsError`), and each of
  its codes a subclass (`kitchen.NotFoundError`) with a static `CODE` and the
  IDL message as its default message. Payload fields become properties. A
  code without fields is constructed as `new kv.InvalidPathError(message?)`;
  a code with fields takes them first:
  `new kv.RejectedError({ key, reason }, message?)`.
- A cancelled call rejects with `CancelledError` (code -5).
- Runtime failures throw the root class itself: -1 generic, -2 producer
  panic, -3 marshalling failure, -4 a callback implementation failed.

Only functions declared `throws` map codes onto their module's domain; a
failure of any other call is a producer bug, and the binding throws the
root class with the code and the producer's message (see
[the trap policy](../guides/errors-and-memory.md#the-trap-policy)).

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
| direct (numbers, `bool`, C-style enums) | the value | the value |
| `string`, `bytes` | a `string`, a `Uint8Array` | a copy in a run the library adopts |
| record, rich enum, `T?`, `[T]`, `{K:V}` | the value | its encoding, in a run the library adopts |
| `I` | a wrapper (never `null`) | a new strong reference to its object |
| `I?` | a wrapper or `null` | a new strong reference, or null |

The implementation keeps its own wrapper; the library gets a reference of
its own. A method that throws, or returns a value of the wrong type, fails
the native call with code -4 and the exception's message; nothing unwinds
through native code. A method declared `throws` may instead throw an error
of its module's domain, and the library receives that code, message, and
fields:

```js
const policy = {
  admit(entry) {
    if (entry.key.startsWith('secret')) {
      throw new kv.RejectedError({ key: entry.key, reason: 'no secrets' }, 'secrets are not stored');
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

An `iter<T>` result is a lazy iterator: each `next()` makes one native call.
The native iterator is destroyed when it's exhausted, when it fails, on
`return()` (which `for...of` calls on `break`), on `close()`, or when the
iterator is garbage collected.

## Known limitations

- A callback from another thread fails with -4 if it waits for a second on
  a synchronous call (see [Threading](#threading)).
- A callback implementation that holds a wrapper of the object retaining it
  forms a cycle the garbage collector can't see; close the wrapper or
  release the callback explicitly.
- `{PREFIX}_LIBRARY` selects the library at build time, not at run time.
- Windows builds link `{library}.lib`, and the DLL must be on the loader's
  search path.
- The package is ESM only, and its declarations need TypeScript 5.2 or
  later (for `Symbol.dispose`).
