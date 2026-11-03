# Node.js

The Node.js target generates an npm package: an ES module with TypeScript
declarations that loads the producer's shared library with
[koffi](https://koffi.dev) and calls its [C ABI](../reference/abi.md)
(revision 6) directly. There's no generated C, no addon, and nothing to
compile at install time. The JavaScript API is shared with the
[WebAssembly target](wasm.md); only the transport underneath differs, so
code written against one runs against the other.

Node.js is a [Tier 1](../stability.md#target-tiers) target: it tracks every
ABI revision as it lands and runs the full conformance suite in CI.

## What gets generated

For a library named `kvstore` (the snapshot fixtures use `kitchen_sink`):

```text
node/
├── package.json   ES module manifest, depending on a pinned koffi release
├── index.js       the C signatures, the load-time checks, and the API
├── index.d.ts     TypeScript declarations
├── runtime.js     the shared runtime (codec, checks, errors, wrappers, load-time checks)
├── native.js      the transport: loading the library with koffi and binding its symbols
├── debug.js       the `./debug` export: leak counters for tests
├── debug.d.ts
└── README.md
```

The package name defaults to the package identity's `name`; set `name` under
`[generators.node]` in `weaveffi.toml` to override it, and `node_engine` to
change `engines.node` (default `>=18`). The package's one dependency is
koffi, pinned to an exact release (3.3.2).

## Install and load

The generated directory is a complete package. Install it like any other,
and npm fetches koffi (which ships prebuilt binaries for every platform it
supports):

```sh
npm install ./node
```

Importing the package loads the library. The first of these that exists
wins:

1. the path in the `{PREFIX}_LIBRARY` environment variable
   (`KVSTORE_LIBRARY`), the library file or the directory holding it;
2. the copy in the `{package}-{os}-{cpu}` package npm installed for this
   platform (see [Packaging](#packaging));
3. a copy next to `index.js`, where `weaveffi generate --dev` puts it;
4. the bare file name (`libkvstore.so`, `libkvstore.dylib`, `kvstore.dll`),
   which the system's library search resolves (`LD_LIBRARY_PATH`,
   `DYLD_LIBRARY_PATH`, `PATH`).

```js
import { Store, EntryKind, KvstoreError } from 'kvstore';

const store = Store.open('/tmp/data');
store.put('greeting', new TextEncoder().encode('hi'), EntryKind.Volatile, null);
store.close();
```

If no candidate loads, the import throws an `Error` that lists the paths it
tried and names the environment variable.

## Load-time checks

Before any other call, the import checks that the library matches the
bindings (see [Load-time checks](../reference/abi.md#load-time-checks)): the
library must implement ABI revision 6, and it must export every
declaration's contract function, returning the hash the bindings were
generated with. `index.js` embeds one row per declaration, error code, and
callback method, each with its canonical signature:

```js
const $contract = [
  ['kitchen_sink_contract_784cfd093415f2b5', 0xffcf2bd3b04aa002n, 'Gadget.new'], // constructor new(i64) -> Gadget
  // ...
];
$verify($m, 'kitchen_sink', 6, $contract);
```

If a check fails, the import throws an `Error` naming the declaration:
`kvstore: Store.put is missing from the library` or `kvstore: Store.put
changed since these bindings were generated`. Declarations the library has
and the bindings don't are fine, so a library that only adds to its API
(declarations, error codes, callback methods) keeps working with older
bindings. Since the import fails the same way when the library can't be
found, `await import(...)` in a `try` can fall back.

## Names

A library is one namespace, and every declaration is a named export of the
package: classes for interfaces and errors, frozen objects for C-style
enums, functions, and types for records, rich enums, and callback
interfaces.

```js
import * as kitchen from 'kitchen_sink';

const gadget = new kitchen.Gadget(1n);
kitchen.optI32Id(null);
```

Functions, methods, and parameters are camelCase. A free function or
parameter whose name is a JavaScript reserved word gains a trailing
underscore (a function `delete` becomes `delete_`), and so does a free
function named `init`, which the WebAssembly target exports, so the two
APIs stay the same. Methods keep reserved words, which are legal property
names (`store.delete(key)`), except that an instance method named `close` or
`constructor`, and a static named `name`, `length`, `prototype`, or
`caller`, gains one too
([reserved member names](../reference/naming.md#identifiers-in-generated-code)).
Record fields keep their IDL spelling. Doc comments in `index.d.ts` name
declarations in this spelling too: an IDL doc or deprecation message that
says `` `new_op` `` reads `` `newOp` `` there.

Inside `index.js`, each declaration is bound under its public name plus `$`
(`Gadget$`) and exported under the public name, so no declaration can
shadow a global the generated code uses, such as `Map` or `TypeError`.

## Type mapping

| IDL type | TypeScript type | Notes |
|----------|-----------------|-------|
| `i8`, `i16`, `i32`, `u8`, `u16`, `u32` | `number` | A fractional, non-finite, or out-of-range argument throws a `RangeError`; nothing wraps |
| `i64`, `u64` | `bigint` | Integral numbers in range are accepted as arguments; anything else throws a `RangeError` |
| `f32`, `f64` | `number` | NaN, the infinities, and `-0` round-trip; an `f32` argument rounds as `Float32Array` does |
| `bool` | `boolean` | |
| `string` | `string` | UTF-8 in a run, so interior NULs survive |
| `bytes` | `Uint8Array` | |
| record | `interface` | A plain object; fields keep their IDL names |
| C-style enum | `enum` | A frozen object with forward and reverse mappings (negative values too) |
| rich enum | tagged union | `{ tag: 'Circle', radius: 2.5 }` |
| interface | `class` | See [Objects and lifetime](#objects-and-lifetime) |
| callback interface | `interface` | Any object with the methods; see [Callback interfaces](#callback-interfaces) |
| `T?` | `T \| null` | `undefined` is accepted as `null`. An optional scalar or C-style enum crosses directly as a `{prefix}_opt_*` struct, with no buffer |
| `[T]` | `T[]` | A list of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`, or `f64` crosses as a typed array: an argument may be an array (checked element by element) or the matching typed array (`Float64Array` for `[f64]`, passed as is), and a result is a plain array |
| `{K: V}` | `Record<string, V>` | `Record<number, V>` for integer keys of 32 bits or fewer, `Partial<Record<E, V>>` for C-style enum keys; keys are property names (strings) at run time, and a `Map` is accepted as an argument |
| `iter<T>` | `NativeIterator<T>` | Lazy; an `IterableIterator<T>` with `close()` and `[Symbol.dispose]()` |

Arguments are checked before the native call: a value of the wrong type
throws a `TypeError`, and an integer that is fractional or out of its type's
range a `RangeError`, whether it's an argument, an element of a list, a
field of a record, or a callback's return value.

Value buffers are encoded and decoded by one function per type: each record
and rich enum (`$w$Entry`, `$r$Entry`), each interface carried as an object
token, and each distinct optional, list, and map type in the API, named by
its canonical stem (`$w_list_opt_Entry` writes `[Entry?]`). Call sites name
these functions.

## How values cross

`index.js` lists the C signature of every function it calls (`$sigs`), one
slot kind per C value: a scalar (`i32`, `bool`, ...), an optional scalar
(`opt_i64`), a run (`run`), a pointer (`ptr`), a callback-interface argument
(`cb`), and the slots the transport fills in itself (the error slot `err`,
an iterator's item slot `out:<kind>`, and an async launcher's completion
`done:<kind>` and context `ctx`):

```js
const $sigs = {
  kitchen_sink_opt_mixed: ['opt_u16', ['f64', 'opt_f32', 'opt_i64', 'run', 'err']],
  kitchen_sink_do_cancellable: ['void', ['run', 'ptr', 'done:run', 'ctx']],
  kitchen_sink_StreamItemsIterator_next: ['i32', ['ptr', 'out:run', 'err']],
  // ...
};
```

`native.js` declares the C structs (`{prefix}_run`, the `{prefix}_opt_*`
structs, `{prefix}_error`, and the callback-interface argument) with koffi's
type syntax and binds each function by value, exactly as the header declares
it. A bound function takes and returns one JavaScript value per C value: a
`Uint8Array` for a run (lent for a parameter, and a copy for a return, whose
run it then releases with `{prefix}_run_free`), the value or `null` for an
optional scalar, a `bigint` handle or `null` for an object. After a
synchronous call it reads the error slot, and on a failure it clears the
slot and throws. The generated API above it does the rest: it encodes
strings as UTF-8, checks and copies typed arrays, encodes and decodes
buffers, and lends, adopts, and clones objects.

```js
  poke(times) {
    const $self = $lend(this, Gadget$);
    try {
      const $a0 = $check.I32(times, 'times');
      return $f.kitchen_sink_Gadget_poke($self, $a0);
    } catch ($e) {
      throw $from$KitchenErrors($e);
    } finally {
      $unlend(this);
    }
  }
```

A typed-array argument is lent to the library in place: its elements are
aligned for their type, since a typed array's offset is always a multiple of
its element size.

## Objects and lifetime

An interface is a class whose instances each own one strong reference to the
native object. `close()` releases it, and so does `[Symbol.dispose]()` (for
`using` declarations); both are idempotent, and a wrapper that is garbage
collected unclosed is released by a `FinalizationRegistry`. Every call lends
the wrapper to the native side for its duration, so a `close()` that races
the call (from a callback, or while an async method is pending) waits for it
to return. Using a closed wrapper throws a code -3 error.

The synchronous constructor named `new` is the class constructor; other
constructors are static factories, and a class without one can't be
constructed directly. Objects returned, awaited, iterated, decoded from a
record, or passed to a callback method are new wrappers the receiver owns.
Two wrappers may share one native object.

## Errors

Every error extends the package's root class, named after the package
(here `KitchenSinkError`), which carries the ABI `code`. When the package's
own name is also a declaration's (the `codec` sample's error domain is
`CodecError`), the root class adds `Library` (`CodecLibraryError`).

- Each error domain is a class, named by the shared rule (a domain
  `KitchenErrors` is `KitchenError`, `KvError` stays `KvError`, and
  `Failure` becomes `FailureError`), and each of its codes a subclass
  (`NotFoundError`) with a static `CODE` and the IDL message as its default
  message. Codes of different domains may share a value. Payload fields
  become properties. A code without fields is constructed as
  `new InvalidPathError(message?)`; a code with fields takes them first:
  `new RejectedError({ key, reason }, message?)`. A code whose class would
  take a name another export has gains its domain's name first.
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

An async function returns a `Promise`. Its completion is a JavaScript
function koffi registered once per result type, and it may arrive on any
producer thread; koffi runs it on the JavaScript thread, and it settles the
promise. While any call is pending, the transport keeps the event loop
alive, so a script that awaits nothing else still waits for it. A
cancellable function takes an optional last argument with an `AbortSignal`:

```ts
export declare function doCancellable(input: string, options?: { signal?: AbortSignal }): Promise<string>;
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

An implementation crosses as the `{prefix}_{Cb}` struct: a context, an
integer the package maps back to the implementation, and the interface's
static vtable. The vtable is built once per interface, the first time an
implementation is passed, from JavaScript functions registered with koffi
(`free` first, then one per method); it lives as long as the process, so the
number of registered callbacks stays small however many implementations are
live. The library's call to `free` releases the implementation.

A method's return value is checked and handed to the library in its family's
form:

| IDL return | Return from JavaScript | What the library receives |
|------------|------------------------|---------------------------|
| direct (numbers, `bool`, C-style enums) | the value | the value (integers range-checked) |
| optional scalar (`i32?`, `Mode?`, ...) | the value, or `null` | a `{prefix}_opt_*` struct |
| numeric list (`[f64]`, ...) | an array or the matching typed array | a copy in a run from `{prefix}_alloc`, which the library adopts |
| `string`, `bytes` | a `string`, a `Uint8Array` | a copy in a run the library adopts |
| record, rich enum, other `T?`, `[T]`, `{K:V}` | the value | its encoding, in a run the library adopts |
| `I` | a wrapper (never `null`) | a new strong reference to its object |
| `I?` | a wrapper or `null` | a new strong reference, or null |

Nothing unwinds through native code; how a failure reaches the library
follows the method's `throws`:

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
      throw new RejectedError({ key: entry.key, reason: 'no secrets' });
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
on the JavaScript thread:

- A call made on the JavaScript thread (because the JavaScript thread is
  inside a call into the library) runs directly, nested inside that call.
- A call made on any other thread is queued by koffi for the JavaScript
  thread and runs from its event loop, while the producer thread waits for
  it to return. Nothing runs while the JavaScript thread is busy, so such
  calls are delivered only once the current JavaScript task returns to the
  event loop.
- The library's release of an implementation (its vtable's `free`) may come
  from any thread and follows the same rules.

The waiting in the second rule can deadlock: if the JavaScript thread is
inside a synchronous call into the library that itself waits for the thread
making the callback (joins it, or waits on a lock it holds), neither thread
can proceed. So:

- Make calls that wait for callbacks from other threads `async`, or have
  the producer call back on the calling thread.
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

## Packaging

`weaveffi package` writes npm tarballs in the esbuild-style layout: one
`{package}-{os}-{cpu}` package per desktop platform, gated by npm `os` and
`cpu` and carrying only that platform's library, plus the main package,
which lists them in `optionalDependencies`. `npm install` then fetches the
main package, koffi, and the one platform package that matches, and the
import loads the library from it. See [Packaging](../guides/packaging.md).

## Leak counters

The package's `./debug` export reads the library's live-resource counters,
for tests of code that uses the bindings. It isn't part of the API:

```js
import { debugLive } from 'kvstore/debug';

debugLive(0); // live objects, as a bigint (1 callbacks, 2 iterators, 3 cancel tokens, 4 runs)
```

Kind -1 is `1n` when the library counts at all (built with `weaveffi`'s
`leak-check` feature); otherwise every counter is `0n`.

## Known limitations

- A callback from another thread waits for the JavaScript thread, so a
  synchronous call that waits for that callback's thread deadlocks (see
  [Threading](#threading)).
- A callback implementation that holds a wrapper of the object retaining it
  forms a cycle the garbage collector can't see; close the wrapper or
  release the callback explicitly.
- The package runs on the platforms koffi supports, which ships prebuilt
  binaries for the desktop and server platforms Node.js runs on.
- A numeric list result is copied into a plain array. As an argument, the
  matching typed array is lent to the library without a copy, while a plain
  array is checked and copied first.
- The package is ESM only, and its declarations need TypeScript 5.2 or
  later (for `Symbol.dispose`).
