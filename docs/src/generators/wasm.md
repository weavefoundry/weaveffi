# Wasm

## Overview

The Wasm target produces a typed ES module loader for
`wasm32-unknown-unknown` builds of WeaveFFI cdylibs, speaking ABI
revision 2. The loader wraps the raw exports in idiomatic JavaScript:
per-module namespaces, plain JS objects for records and tagged objects
for rich enums, reference-counted object wrappers with `close()` and
`Symbol.dispose`, `Promise`-based async functions, lazy iterators,
callback interfaces implemented as plain JS objects, thrown `Error`s
instead of error slots, and automatic string/bytes staging in linear
memory. TypeScript declarations describe the whole surface.

C and C++ producers compiled with Emscripten are supported through a
dedicated loader variant; see [Emscripten mode](#emscripten-mode).

Callback interfaces and async functions deliver **synchronously, on
the calling thread**: a `wasm32-unknown-unknown` module is
single-threaded, so a callback runs only while a call into the module is
on the stack; see [Callback interfaces](#callback-interfaces).

## What gets generated

| File | Purpose |
|------|---------|
| `generated/wasm/weaveffi_wasm.js` | ES module: memory helpers, value-buffer codecs, object and iterator runtime, and the async `loadWeaveffiWasm(source)` loader returning typed bindings |
| `generated/wasm/weaveffi_wasm.d.ts` | TypeScript declarations for the loader, every module namespace, object class, and callback interface |
| `generated/wasm/package.json` | npm package manifest (`type: "module"`) |
| `generated/wasm/README.md` | Quickstart and boundary conventions |

## Configuration

```toml
[generators.wasm]
module_name = "weaveffi_wasm"   # basename of the emitted .js/.d.ts
emscripten = false              # see Emscripten mode
allow_unsupported = false       # emit throwing stubs instead of failing in Emscripten mode
```

`prefix` is also accepted and defaults to `[global] c_prefix`, so the
glue calls the same exported symbols the producer emits.

## Type mapping

| IDL type     | Wasm boundary | JavaScript surface |
|--------------|---------------|--------------------|
| `i32` / `u32`| `i32`         | `number`           |
| `i8` / `i16` | `i32`         | `number`           |
| `u8` / `u16` | `i32`         | `number`           |
| `i64`        | `i64`         | `bigint`           |
| `u64`        | `i64`         | `bigint`           |
| `f64`        | `f64`         | `number`           |
| `f32`        | `f32`         | `number`           |
| `bool`       | `i32`         | `boolean` (0/1 at the boundary) |
| `string`     | `i32` pointer (NUL-terminated UTF-8) | `string`, staged via `weaveffi_alloc` |
| `bytes`      | `i32` pointer + `i32` length | `Uint8Array` copy |
| `InterfaceName` | `i32` pointer into linear memory; one strong reference per wrapper | `class` with `close()` and `Symbol.dispose` |
| `InterfaceName?` | `i32` pointer, 0 = absent | `InterfaceName \| null` |
| `CallbackName` | `i32` context key + `i32` pointer to a static vtable | any object with the interface's methods |
| `StructName` | value buffer (`i32` pointer + `i32` length) | plain JS object |
| `EnumName` (plain, C-style)   | `i32` discriminant | `number` (frozen constants object) |
| `EnumName` (rich / algebraic) | value buffer | tagged plain object (`{ tag: "Circle", radius: 2 }`) |
| `T?` (non-object) | value buffer | `T \| null` |
| `[T]`        | value buffer  | `Array` copy |
| `{K: V}`     | value buffer  | plain object (`Map` accepted on input) |
| `iter<T>`    | iterator handle + `next` out-slot | lazy `IterableIterator<T>` |

## Example IDL → generated code

The loader exports a single async entry point that compiles,
instantiates, and wraps a module. It accepts a URL to fetch, the bytes,
or an already compiled `WebAssembly.Module`, and checks the ABI revision
first:

```javascript
import { loadWeaveffiWasm } from './weaveffi_wasm.js';

const api = await loadWeaveffiWasm('/your_library.wasm');
```

```js
const _ABI_VERSION = 2;

function _checkAbiVersion(wasm) {
  if (typeof wasm.weaveffi_abi_version !== 'function') {
    throw new Error(`the loaded WeaveFFI module predates ABI versioning (this glue expects ABI revision ${_ABI_VERSION})`);
  }
  const found = wasm.weaveffi_abi_version() >>> 0;
  if (found !== _ABI_VERSION) {
    throw new Error(`WeaveFFI ABI mismatch: this glue expects revision ${_ABI_VERSION} but the loaded module reports revision ${found}`);
  }
}
```

Functions are grouped by IDL module in lowerCamelCase (nested IDL
modules nest namespaces, e.g. `api.kv.stats.getStats(store)`), object
classes hang off their module namespace (`api.kv.Store`), and error
classes and plain-enum constant objects are top-level named exports:

```javascript
import { loadWeaveffiWasm, KvError, KeyNotFound, EntryKind } from './weaveffi_wasm.js';

const api = await loadWeaveffiWasm('/kvstore.wasm');
const store = api.kv.Store.open('/tmp/cache.kv');
store.put('alpha', new Uint8Array([1]), EntryKind.Volatile, null);
for (const key of store.listKeys(null)) console.log(key);
store.close();
```

Records come back as plain JS objects, serialized in a value buffer at
the boundary and packed/unpacked by generated per-record codecs. From
the `kvstore` sample:

```js
// Decode a `kv.Entry` record from the value-buffer wire format.
function _read_kv_Entry(r) {
  const v = {};
  v.id = r.i64();
  v.key = r.str();
  v.value = r.bytes();
  v.created_at = r.i64();
  v.expires_at = (r.flag() ? r.i64() : null);
  v.tags = (() => { const _n = r.len(); const _arr = []; for (let _i = 0; _i < _n; _i++) _arr.push(r.str()); return _arr; })();
  v.metadata = (() => { const _n = r.len(); const _obj = {}; for (let _i = 0; _i < _n; _i++) { const _k = r.str(); _obj[_k] = r.str(); } return _obj; })();
  return v;
}
```

The raw exports stay reachable for anything not covered by the typed
surface:

```javascript
api._raw.weaveffi_alloc(16);
```

## Typed errors

The module exports `WeaveFFIError` (extending `Error` with a numeric
`code`). A module's error domain adds an exported base class named after
the domain plus one exported class per code, each carrying its stable
`CODE` and reachable both flat and via the domain class. From the
`kvstore` sample:

```js
/** Base error for WeaveFFI failures: domain errors extend it, and it is
 * thrown directly for unknown codes, marshalling failures, producer
 * panics, and callback-interface implementations that raised (code -4).
 * Carries the stable ABI `code`. */
export class WeaveFFIError extends Error {
  constructor(code, message) {
    super(message ? `WeaveFFI error ${code}: ${message}` : `WeaveFFI error ${code}`);
    this.name = new.target.name;
    this.code = code;
  }
}

/** Base error for the `kv` module's error domain. */
export class KvError extends WeaveFFIError {}

// key not found
export class KeyNotFound extends KvError {
  constructor(message = "key not found") {
    super(1001, message);
  }
}
KeyNotFound.CODE = 1001;
KvError.KeyNotFound = KeyNotFound;
// Expired, StoreFull, IoError follow the same shape.
```

A callable with `throws: true` checks the error slot through the
domain's mapper (`_kvErrorFrom`), so a failure arrives as the matching
subclass (`KeyNotFound`), the domain (`KvError`), or, for codes outside
the domain, the generic `WeaveFFIError`. A callable without `throws`
uses the generic checker only; a non-zero code there can only be a
producer panic, a marshalling failure, or a callback implementation that
threw, and it surfaces as `WeaveFFIError`. There is no separate trap
idiom on this target: JavaScript has exceptions, so both paths throw.

An error code that declares payload `fields:` carries them serialized
in the error's payload buffer; the checker decodes them onto the thrown
error object before clearing the slot.

### Runtime error codes

| Code | ABI name | When you see it on this target |
|------|----------|--------------------------------|
| `-1` | `GENERIC_ERROR_CODE` | A producer reported a failure without a domain code; a `#[weaveffi::export]` body that returns a bare `Err(String)`. |
| `-2` | `PANIC_ERROR_CODE` | The producer panicked. `wasm32-unknown-unknown` has no unwinding runtime: a panic executes `unreachable`, the engine throws `WebAssembly.RuntimeError`, and the wrapper's `catch` translates it (`_trap`/`_trapError`) into `WeaveFFIError(-2, 'producer panicked: ...')`. The producer frames are not unwound, so a lock the producer held across the call stays locked. |
| `-3` | `MARSHAL_ERROR_CODE` | The consumer passed something the boundary can't carry, most often a closed object wrapper (`_borrow` throws `WeaveFFIError(-3, 'expected a live object wrapper')`), or the producer returned a malformed value buffer (`_BufReader` reports a truncated buffer or invalid UTF-8). |
| `-4` | `FOREIGN_ERROR_CODE` | A callback-interface method you implemented threw. The trampoline records the failure on the producer's `out_err`; the wrapper you called throws `WeaveFFIError` with code `-4` and your exception's message. |

Errors surface wherever the call was made: thrown from synchronous
methods and free functions, as a rejected `Promise` from async
functions, and thrown from `next()` (or the `for...of` loop) for
iterators.

## Objects (interfaces)

An `interfaces:` entry becomes a class exposed on its module's namespace
(`api.kv.Store`). A constructor named `new` becomes the JS
`constructor`; other constructors are static factories; methods are
camelCased instance methods; statics are static methods. Each wrapper
owns exactly one strong reference to the producer object, and `close()`
releases it. From the `kvstore` sample (trimmed):

```js
class Store {
  static _wrap(handle) {
    return _adopt(Object.create(Store.prototype), handle, wasm.weaveffi_kv_Store_destroy);
  }
  _clone() {
    return wasm.weaveffi_kv_Store_clone(_borrow(this));
  }
  close() {
    _release(this);
  }
  [_dispose]() {
    _release(this);
  }
  static open(path) {
    const [a0_p, a0_s] = _cstr(wasm, path);
    const _err = _allocErr(wasm);
    let _r;
    try {
      _r = wasm.weaveffi_kv_Store_open(a0_p, _err);
    } catch (e) {
      throw _trap(wasm, _err, e);
    } finally {
      wasm.weaveffi_dealloc(a0_p, a0_s);
    }
    _checkKvError(wasm, _err);
    _freeErr(wasm, _err);
    return Store._wrap(_r);
  }
  share() { /* ... */ return Store._wrap(_r); }
  larger(other) {
    /* ... */
    _r = wasm.weaveffi_kv_Store_larger(_borrow(this), (other === null || other === undefined ? 0 : _borrow(other)), _err);
    /* ... */
    return _r === 0 ? null : Store._wrap(_r);
  }
}
```

The shared runtime behind those methods:

```js
// Bind one strong reference to a wrapper and arm the backstop.
function _adopt(obj, handle, destroy) {
  obj._handle = handle;
  obj._destroy = destroy;
  if (_finalizer !== null) _finalizer.register(obj, [destroy, handle], obj);
  return obj;
}

// Release a wrapper's reference exactly once; later calls are no-ops.
function _release(obj) {
  if (!obj._handle) return;
  if (_finalizer !== null) _finalizer.unregister(obj);
  obj._destroy(obj._handle);
  obj._handle = 0;
}

// The pointer of a live wrapper, lent to the producer for one call (the
// wrapper keeps its own reference). A closed or non-object argument is a
// consumer programming error, reported with the marshalling code.
function _borrow(obj) {
  if (obj === null || obj === undefined || !obj._handle) {
    throw new WeaveFFIError(-3, 'expected a live object wrapper');
  }
  return obj._handle;
}
```

- **Disposal.** `close()` calls the producer's `_destroy` symbol and
  drops one reference; it is idempotent. The same method is reachable as
  `[Symbol.dispose]`, so `using store = api.kv.Store.open(path)` releases
  it at block exit on runtimes with explicit resource management (older
  runtimes get `Symbol.for('Symbol.dispose')` as a stand-in). A
  `FinalizationRegistry` backstop releases wrappers that are garbage
  collected without `close()`; on runtimes without
  `FinalizationRegistry`, `close()` is the only release path and a
  forgotten wrapper keeps its reference until the module instance is
  dropped. Don't rely on the backstop for timely release.
- **Use after close.** Every method and every parameter site goes
  through `_borrow`, so touching a closed wrapper throws
  `WeaveFFIError` with code `-3`, synchronously for methods and as a
  rejection for async methods.
- **Passing objects lends, returning objects owns.** A wrapper passed as
  a parameter lends its pointer for the duration of the call and keeps
  its own reference. A returned pointer is adopted into a fresh wrapper
  that owns that reference. `share()` in the sample returns a second
  wrapper around the same producer object; both must be closed.
- **Copies mint a new strong reference.** When a wrapper is placed
  inside a value buffer (a record field, a list element, an optional)
  the codec calls `_clone()`, which invokes the producer's `_clone`
  symbol, so the buffer carries its own reference and the producer
  adopts it:

```js
// Serialize a `kv.StoreInfo` record into the value-buffer wire format.
function _write_kv_StoreInfo(w, v) {
  w.str(v.label);
  w.obj(v.store._clone());
  if (v.mirror === null || v.mirror === undefined) {
    w.flag(false);
  } else {
    w.flag(true);
    w.obj(v.mirror._clone());
  }
  w.i64(v.count);
}

// Decode a `kv.StoreInfo` record from the value-buffer wire format.
function _read_kv_StoreInfo(r) {
  const v = {};
  v.label = r.str();
  v.store = Store._wrap(r.obj());
  v.mirror = (r.flag() ? Store._wrap(r.obj()) : null);
  v.count = r.i64();
  return v;
}
```

- **Nullable objects** (`Store?`) are `Store | null`: `null` or
  `undefined` becomes a 0 pointer on input, and a 0 return becomes
  `null` (`larger` above).
- **Lists of objects** decode into arrays of fresh wrappers, each owning
  one reference (`Store.openMany(paths)` returns
  `_arr.push(Store._wrap(_rd.obj()))` per element); close each one.
- **Iterators over objects** yield a fresh owning wrapper per `next()`.
- **Async functions returning objects** resolve the `Promise` with a
  fresh owning wrapper.

The TypeScript declarations document the ownership rule at every object
return:

```typescript
/**
 * A second reference to this same store (the returned pointer equals
 * the receiver's; both must eventually be destroyed).
 * @returns A wrapper owning one reference; `close()` it (or bind it with `using`) when done.
 * @throws {WeaveFFIError} if the native call fails
 */
share(): Store;
larger(other: Store | null): Store | null;
describe(label: string, mirror: Store | null): StoreInfo;
```

## Callback interfaces

A `callback_interfaces:` entry becomes a TypeScript `interface`; the
consumer implements it with any object that has the methods (a class
instance or an object literal, duck-typed). From the `events` sample's
`weaveffi_wasm.d.ts`:

```typescript
export interface Subscriber {
  /** Decide how the bus should treat `topic` for this subscriber. */
  route(topic: string): Delivery;
  /** Receive an accepted message. Returns the subscriber's running count
   * of received messages. */
  onMessage(message: Message): bigint;
  /** Receive the bus itself (an object handed through a callback). The
   * consumer adopts the reference and may keep or drop it. */
  onAttached(bus: EventBus): void;
}
```

```js
const received = [];
const sub = {
  route(topic) { return topic.startsWith('alerts/') ? Delivery.Accept : Delivery.Skip; },
  onMessage(message) { received.push(message.text); return BigInt(received.length); },
  onAttached(bus) { bus.close(); },   // owned by the implementation
};
const bus = new api.events.EventBus();
bus.subscribe(sub);
bus.publish('alerts/disk', 'low space', []);   // route and onMessage run before publish returns
```

Under the hood the loader registers the implementation in a `Map` keyed
by an integer context id and passes that id plus a pointer to a static
vtable in linear memory. The vtable is filled once per loaded instance
with function-table indices of `WebAssembly.Function` trampolines, one
per method plus `free`:

```js
const _cb_Subscriber_on_attached = _registerTrampoline(_table, ['i32', 'i32', 'i32'], [], (_ctx, a0, _err) => {
  try {
    const _impl = _callbacks.get(_ctx);
    const _p0 = EventBus._wrap(a0);
    if (_pendingForeign !== null) {
      _reportForeign(wasm, _err, _pendingForeign);
      return;
    }
    _impl.onAttached(_p0);
  } catch (e) {
    _setForeignError(wasm, _err, e);
  }
});
// `free(ctx)`: the producer dropped its last reference; forget the
// implementation so it can be collected.
const _cb_Subscriber_free = _registerTrampoline(_table, ['i32'], [], (_ctx) => { _callbacks.delete(_ctx); });
const _vtable_Subscriber = wasm.weaveffi_alloc(16);
{
  const _dv = new DataView(wasm.memory.buffer);
  _dv.setUint32(_vtable_Subscriber + 0, _cb_Subscriber_route, true);
  _dv.setUint32(_vtable_Subscriber + 4, _cb_Subscriber_on_message, true);
  _dv.setUint32(_vtable_Subscriber + 8, _cb_Subscriber_on_attached, true);
  _dv.setUint32(_vtable_Subscriber + 12, _cb_Subscriber_free, true);
}
```

- **Argument ownership.** Strings, bytes, and buffered values (records,
  rich enums, optionals, lists, maps) are borrowed from the producer for
  the duration of the dispatch and decoded into fresh JavaScript values
  before your method runs, so they're safe to retain. An object argument
  (`onAttached(bus)`) is **owned by the implementation**: the trampoline
  adopts the reference into a wrapper, and you `close()` it when you're
  done (or keep it; the `FinalizationRegistry` backstop applies).
- **Lifetime.** The implementation stays registered until the producer
  drops its last reference and calls the vtable's `free`, which removes
  the `Map` entry. Replacing a listener (`setEvictionListener`) frees the
  previous one.
- **When the implementation throws.** The trampoline catches the
  exception, writes `FOREIGN_ERROR_CODE` (`-4`) and the message into the
  producer's `out_err`, and returns the method's default value. Because
  `wasm32` can't unwind, the producer keeps running to completion; the
  message is parked in `_pendingForeign` so any further callback during
  that same call is refused with the same error instead of reaching your
  implementation again. When the producer call returns, the wrapper you
  called throws `WeaveFFIError` with code `-4` carrying your message
  (async: the `Promise` rejects). The object stays usable afterwards; the
  conformance consumer keeps publishing on the same bus. A method that
  returns the wrong type (a non-`bigint`-coercible value from
  `onMessage`) surfaces the same way.
- **Thread affinity.** The target is single-threaded, so a callback runs
  only while a call into the module is on the stack, before that call
  returns. A producer that calls back from a spawned thread cannot run
  on `wasm32-unknown-unknown` at all (`std::thread::spawn` fails there).
  There is no re-entrancy hazard beyond the ordinary JavaScript one:
  calling back into the same object from inside your callback is a
  synchronous nested call.

## Rich (algebraic) enums

A *rich* (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style enum stays an `i32` discriminant (surfaced as a
`number` plus a frozen constants object), but a rich enum is a plain
object tagged by variant name (`{ tag: "Circle", radius: 2 }`) that
crosses the boundary serialized in a value buffer, exactly like a
record: an `i32` tag followed by the active variant's fields.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`, the generated
`weaveffi_wasm.d.ts` types the value as a discriminated union:

```typescript
export type Shape =
  | { tag: "Empty" }
  | { tag: "Circle"; radius: number }
  | { tag: "Rectangle"; width: number; height: number }
  | { tag: "Labeled"; label: string; count: number };
```

Construct values as object literals and branch on `tag`; TypeScript
narrows the union automatically:

```js
const circle = { tag: 'Circle', radius: 2.0 };
console.log(api.shapes.describe(circle));
const bigger = api.shapes.scale(circle, 3.0); // a fresh Shape object
```

Values are plain JavaScript data with nothing to free. A variant field
of interface type follows the object rule: `_clone()` on the way in, a
fresh owning wrapper on the way out.

## 64-bit integers and floats

`i64` and `u64` are `bigint` everywhere: as parameters, returns, record
fields, and callback returns (`onMessage(...): bigint`). The value-buffer
codec reads them with `getBigInt64`/`getBigUint64` and writes them with
`BigInt(v)`, so a `number` is accepted on input but the full 64-bit range
only round-trips as `bigint`. From the `codec` sample's declarations:

```typescript
i64_value: bigint;
u64_value: bigint;
some_i64: bigint | null;
by_name: Record<string, bigint>;
```

Floats cross as IEEE-754 bit patterns (`setFloat32`/`setFloat64` in
the codec, native `f32`/`f64` params at the boundary), so `NaN`,
`Infinity`, `-Infinity`, and `-0` are preserved; the codec sample's
conformance consumer checks each of them.

## Async support

Async IDL functions return real `Promise`s. The loader grows the
module's `__indirect_function_table` and registers one JavaScript
trampoline per completion-callback signature using the
[JS Type Reflection API](https://github.com/WebAssembly/js-types)
(`new WebAssembly.Function(...)`); each call stores its
`resolve`/`reject` pair in a context map keyed by an integer id. From the
`kvstore` sample:

```js
compact() {
  return new Promise((resolve, reject) => {
    const ctxId = _nextCtxId++;
    _asyncContexts.set(ctxId, { resolve, reject, mkErr: _kvErrorFrom });
    try {
      wasm.weaveffi_kv_Store_compact_async(_borrow(this), 0, _cbPtr_i32_i32_i64, ctxId);
    } catch (e) {
      _asyncContexts.delete(ctxId);
      reject(_trapError(e));
    }
  });
}
```

```js
function _asyncHandler(ctxId, errPtr, ...results) {
  const ctx = _asyncContexts.get(ctxId);
  if (!ctx) return;
  _asyncContexts.delete(ctxId);
  try {
    if (errPtr !== 0) _checkErrRef(wasm, errPtr, ctx.mkErr);
    ctx.resolve(ctx.unwrap ? ctx.unwrap(wasm, ...results) : results[0]);
  } catch (e) {
    ctx.reject(e);
  }
}
```

When the producer invokes the completion callback, the trampoline looks
up the context, settles the promise, and removes the entry. A callable
with `throws: true` stores the module's typed error mapper in the
context (`mkErr`), so the rejection carries the domain error; a
non-throwing async callable rejects with the generic `WeaveFFIError`
for panics, marshalling failures, and callback failures. A result of
object type resolves to a fresh owning wrapper; a buffered result is
decoded into a plain object.

Two caveats apply:

- `WebAssembly.Function` requires a runtime with JS Type Reflection
  (recent V8/SpiderMonkey; Chrome, Firefox, Deno, and Node with
  `--experimental-wasm-type-reflection`).
- The module is single-threaded. The default `weaveffi-abi` spawner
  drives the future inline on `wasm32`, so the completion callback has
  already fired by the time the launcher returns and the `Promise` is
  settled before you `await` it. A producer that installs a spawner
  which defers to another thread cannot run on
  `wasm32-unknown-unknown`.

A `cancellable` function's ABI symbol takes a `weaveffi_cancel_token*`
parameter; the loader passes a null token (the `0` in `compact` above),
so cancellation isn't surfaced on this target. An IDL function that
models cancellation itself is exposed as a plain function in the same
namespace.

## Iterators

`iter<T>` returns are lazy: the wrapper launches the producer iterator
and hands back a `_WeaveFFIIterator` implementing the JS iterator
protocol over the iterator handle. Nothing is drained; each `next()`
issues exactly one producer `next` call through a per-element slot
staged in linear memory. From the `kvstore` sample:

```js
listKeys(prefix) {
  /* ... stage the `string?` prefix in a value buffer ... */
  return new _WeaveFFIIterator(wasm, _it, 4,
    (it, slot, ep) => wasm.weaveffi_kv_Store_ListKeysIterator_next(it, slot, ep),
    (it) => wasm.weaveffi_kv_Store_ListKeysIterator_destroy(it),
    _checkKvError, (w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true)));
}
```

The class settles the handle's lifecycle exactly once: `_close()`
destroys the producer iterator, frees the element slot, and zeroes the
handle. It runs eagerly on exhaustion, on a `next` error or trap, or from
`return()` when iteration stops early; a `for...of` loop calls
`return()` automatically on `break` or `throw`. There is no
finalization backstop for iterators (unlike objects), so abandoning one
without exhausting or closing it leaks the producer handle.

Each decoded element is copied out of linear memory and its producer
allocation released (`_takeCStr` frees strings via
`weaveffi_free_string`); an element of object type is adopted into a
fresh owning wrapper. Errors from the launcher and from each `next`
follow the function's error strategy: a throwing function such as
`Store.listKeys` checks each step with the domain checker and throws the
typed `KvError` subclasses; a non-throwing one throws the generic
`WeaveFFIError` for producer bugs. The TypeScript declaration is
`IterableIterator<T>`.

## Emscripten mode

The default loader fetches a bare `.wasm` and calls
`WebAssembly.instantiate` with an empty import object, which only works
for `wasm32-unknown-unknown` builds. A C or C++ library compiled with
Emscripten needs its own JS runtime, its own import object, and exposes
exports as `Module['_name']` rather than `instance.exports.name`. Set
`emscripten` to generate a loader for that layout:

```toml
# weaveffi.toml
[generators.wasm]
emscripten = true
```

Instead of a URL, the loader accepts the initialized Emscripten module,
or the promise returned by its `MODULARIZE` factory. You construct the
module yourself, so options like `locateFile` stay under your control:

```javascript
import Module from './your_library.js';
import { loadWeaveffiWasm } from './weaveffi_wasm.js';

const api = await loadWeaveffiWasm(Module({ locateFile: (p) => 'build/' + p }));
```

Internally the loader binds the module's underscore-prefixed exports to
the symbol names the glue calls, once, up front:

```javascript
const wasm = {
  // Emscripten replaces HEAPU8 when linear memory grows, so the
  // buffer is re-read on every access instead of captured once.
  get memory() { return { buffer: m['HEAPU8'].buffer }; },
  weaveffi_alloc: m['_weaveffi_alloc'],
  weaveffi_dealloc: m['_weaveffi_dealloc'],
  weaveffi_math_add: m['_weaveffi_math_add'],
  // ...
};
```

Everything after that prologue is identical to the standard loader. The
quoted bracket access on the Emscripten module is deliberate: it
survives Closure Compiler's advanced property renaming, while the rest
of the glue keeps consistent dot access on this locally constructed
object, which Closure can rename safely.

### Building the producer

The generated header tags every export with `{PREFIX}_API`, which
expands to `__attribute__((used, visibility("default")))` under
Emscripten (the same expansion as `EMSCRIPTEN_KEEPALIVE`), so the
symbols survive dead-code elimination without an `-sEXPORTED_FUNCTIONS`
list. The glue stages arguments through `weaveffi_alloc` /
`weaveffi_dealloc`; the generated `weaveffi.c` provides malloc/free-
backed defaults, so compile it into your library or export your own
implementations. A typical build:

```bash
emcc your_library.c generated/c/weaveffi.c -Igenerated/c \
  -o your_library.js \
  -sMODULARIZE=1 -sEXPORT_ES6=1 \
  -sEXPORTED_RUNTIME_METHODS=HEAPU8 \
  -sALLOW_MEMORY_GROWTH=1
```

`-sEXPORTED_RUNTIME_METHODS=HEAPU8` is required: the glue reads and
writes linear memory through `Module['HEAPU8']`.

### Limitations

Async functions and callback interfaces are not supported in Emscripten
mode. The trampoline registration in the standard loader relies on
`WebAssembly.Function` and a growable `__indirect_function_table`,
neither of which an Emscripten module exposes portably. By default
`weaveffi generate` fails when the IDL uses either feature; with
`allow_unsupported = true` each affected entry point becomes an explicit
stub that throws at call time and is omitted from the TypeScript
declarations. Objects and iterators work normally.

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

In Node there is no `file://` fetch; pass the bytes instead:

```bash
node --experimental-wasm-type-reflection consumer.mjs
```

```javascript
import { readFile } from 'node:fs/promises';
const api = await loadWeaveffiWasm(await readFile(new URL('./your_library.wasm', import.meta.url)));
```

## Packaging

`weaveffi package --target wasm` assembles an npm package under `wasm/`
containing the ES-module loader, its `.d.ts`, a README, and the
`wasm32` binary copied in as `<lib_name>.wasm`. `package.json` lists
exactly those files so `npm publish` ships nothing else. Only the
`wasm32` platform slice is consulted; with no `wasm32/` binary in
`--binaries` the target is skipped with a note, and in Emscripten mode
the package is glue only (the consumer links the module into their own
Emscripten build). Build the slice with
`cargo build --release --target wasm32-unknown-unknown` (the producer
crate needs `crate-type = ["cdylib"]`) and place it under
`<binaries>/wasm32/`. See the
[packaging guide](../guides/packaging.md) for the full flow.

## Memory and ownership

- The wrapper stages strings, bytes, and value buffers into linear
  memory with the exported `weaveffi_alloc` / `weaveffi_dealloc` and
  releases them after the call (in a `finally`, so a trap can't leak
  them); you don't manage buffers for typed calls.
- Producer-owned returns (strings, bytes, and value buffers) are copied
  or decoded into JavaScript values and freed via
  `weaveffi_free_string` / `weaveffi_free_bytes` inside the wrapper.
- Records, rich enums, optionals, lists, and maps are plain JavaScript
  values with nothing to free. Object wrappers own one producer
  reference each: `close()` them (or bind with `using`), and let the
  `FinalizationRegistry` backstop catch what you miss.
- Callback implementations are pinned in a `Map` until the producer's
  `free` runs; object arguments handed to them are owned by the
  implementation.
- Error slots are allocated, checked, and cleared internally; failures
  surface as thrown `Error`s with the producer's code and message.
- When you bypass the typed surface via `_raw`, the conventions at the
  top of `weaveffi_wasm.js` apply and every alloc must be paired with a
  dealloc.

## Known limitations

- **Single-threaded delivery.** Callbacks and async completions fire
  only while a call into the module is on the stack. Producers that
  spawn threads or install a deferring spawner can't run on
  `wasm32-unknown-unknown`.
- **Panics don't unwind.** A producer panic traps; the glue reports
  `-2` (or `-4` after a callback failure), but producer frames aren't
  unwound, so state the producer was mutating may be left as is.
- **`WebAssembly.Function` is required** for async functions and
  callback interfaces (Node needs `--experimental-wasm-type-reflection`).
- **No cancellation.** Cancel tokens are passed as null.
- **Iterators have no finalizer.** Exhaust or `return()` every
  iterator; abandoning one leaks the producer handle.
- **Emscripten mode** supports objects and iterators but not async
  functions or callback interfaces.

## Troubleshooting

- **`WebAssembly.Function is not a constructor`**: the runtime lacks
  JS Type Reflection. Use a current Chrome/Firefox/Deno, run Node with
  `--experimental-wasm-type-reflection`, or avoid async functions and
  callback interfaces for this target.
- **`WeaveFFI ABI mismatch`** at load: the `.wasm` was built against a
  different `weaveffi-abi` revision than the glue; regenerate and
  rebuild together.
- **`WeaveFFIError: WeaveFFI error -3: expected a live object wrapper`**:
  a wrapper was used (as receiver or argument) after `close()`.
- **`LinkError: import object field 'env' is not a Function`**: the
  loader instantiates with an empty imports object. If your Rust crate
  imports host functions, extend `loadWeaveffiWasm` to pass them in.
  If the module was built with Emscripten, use
  [Emscripten mode](#emscripten-mode) instead.
- **`WeaveFFI error -2: producer panicked`**: the producer hit a panic
  (on `wasm32` often `SystemTime::now()` or another API the target has
  no host for); the message carries the engine's trap text.
- **A callback never fires**: delivery is synchronous, so callbacks run
  only during one of your calls into the module. There is no background
  delivery on this target.
- **Out-of-memory after many `_raw` calls**: every pointer returned
  from the module must be deallocated; the typed wrappers do this for
  you, raw calls do not.
- **The `.wasm` file fails to instantiate**: the build artifact must
  be `wasm32-unknown-unknown`. `wasm32-wasi` modules require WASI
  imports and cannot run in the browser without a polyfill.
