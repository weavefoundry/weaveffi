# Node.js

The Node.js target generates an npm package: an ES module with TypeScript
declarations, and a small N-API addon that `npm install` compiles with
node-gyp and that calls the library's C ABI. The JavaScript API is shared
with the [WebAssembly target](wasm.md); only the transport underneath
differs, so code written against one runs against the other.

## What gets generated

For a library named `kvstore` (the snapshot fixtures use `kitchen_sink`):

```text
node/
├── package.json      ES module manifest; `npm install` runs `node-gyp rebuild`
├── binding.gyp       builds the addon and links the library
├── kvstore_node.c    the N-API addon
├── kvstore.h         a copy of the C header the addon compiles against
├── index.js          the API: one namespace export per top-level module
├── index.d.ts        TypeScript declarations
├── runtime.js        the shared runtime (codec, errors, wrappers)
└── README.md
```

The package name defaults to the library identity's name; set
`package_name` under `[generators.node]` in `weaveffi.toml` to override it.
The addon is named `{library}_node.node`.

## Install, build, and load

The generated directory is a complete package. Install it like any other,
pointing the build at the native library with the `{PREFIX}_LIBRARY`
environment variable (the library file or its directory) or the matching
npm option:

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

Importing the package loads the addon and checks the contract before any
other call: the library must implement ABI revision 3, and each top-level
module's checksum must match the one the bindings were generated from.
Otherwise the import fails with an error naming the mismatched module.

`weaveffi package` produces the esbuild-style layout instead: the main
package lists one `{package}-{os}-{cpu}` package per platform in
`optionalDependencies`, each gated by npm `os` and `cpu` and carrying the
prebuilt library, and the addon links the one npm installed.

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

Functions and methods are camelCase. A name that is a JavaScript reserved
word gains a trailing underscore (`delete` becomes `delete_`), as does an
instance method named `close` or `constructor` and a static named `name` or
`length`. Record fields keep their IDL spelling.

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
| C-style enum | `enum` | A frozen object with forward and reverse mappings |
| rich enum | tagged union | `{ tag: 'Circle', radius: 2.5 }` |
| interface | `class` | See [Objects](#objects-and-lifetime) |
| callback interface | `interface` | Any object with the methods |
| `T?` | `T \| null` | `undefined` is accepted as `null` |
| `[T]` | `T[]` | |
| `{K: V}` | `Record<K, V>` | Keys are strings at run time; a `Map` is accepted too |
| `iter<T>` | `IterableIterator<T>` | Lazy |

Arguments are type-checked: a value of the wrong type throws a `TypeError`
before the native call.

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
constructed directly. Objects returned, awaited, iterated, or decoded from a
record are new wrappers the caller owns. Two wrappers may share one native
object.

## Errors

Every error extends the package's root class, `{Package}Error` (here
`KitchenSinkError`), which carries the ABI `code`:

- Each error domain is a class (`kitchen.KitchenErrorsError`), and each of
  its codes a subclass (`kitchen.NotFoundError`) with a static `CODE` and the
  IDL message as its default message. Payload fields become properties.
- A cancelled call rejects with `CancelledError` (code -5).
- Runtime failures throw the root class itself: -1 generic, -2 producer
  panic, -3 marshalling failure, -4 a callback implementation failed.

Only functions declared `throws` map codes onto their module's domain.

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
camelCase. Arguments arrive converted (records decoded, objects as wrappers
the implementation owns), and the return value is checked against the IDL
type. A method that throws, or returns a value of the wrong type, fails the
native call with code -4 and the exception's message; nothing unwinds
through native code.

The producer may call back from any thread. A call made on the JavaScript
thread runs directly; one made on another thread runs on the JavaScript
thread while the producer thread waits.

## Iterators

An `iter<T>` result is a lazy iterator: each `next()` makes one native call.
The native iterator is destroyed when it's exhausted, when it fails, on
`return()` (which `for...of` calls on `break`), on `close()`, or when the
iterator is garbage collected.

## Known limitations

- A synchronous call whose producer blocks on a callback from another thread
  deadlocks, because the JavaScript thread is busy with the call.
- A callback implementation that holds a wrapper of the object retaining it
  forms a cycle the garbage collector can't see; close the wrapper or
  release the callback explicitly.
- `{PREFIX}_LIBRARY` selects the library at build time, not at run time.
- Windows builds link `{library}.lib`, and the DLL must be on the loader's
  search path.
- The package is ESM only, and its declarations need TypeScript 5.2 or
  later (for `Symbol.dispose`).
