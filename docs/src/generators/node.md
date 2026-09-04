# Node.js

## Overview

The Node.js target produces a CommonJS loader, TypeScript type
definitions, and the complete N-API addon C source (plus a
`binding.gyp`) that bridges JS to the C ABI. The loader honors a
`WEAVEFFI_ADDON` environment override, then prefers the node-gyp build
output (`./build/Release/weaveffi.node`), and falls back to a prebuilt
binary placed next to it as `index.node`. On top of the raw native
bindings it layers the idiomatic wrappers: error classes, object classes
with `close()` and `Symbol.dispose`, plain-object records and tagged-union
rich enums, duck-typed callback interfaces, `Promise`-returning async
functions, lazy iterators, and camelCased function wrappers. The surface
follows ABI revision 2, and 64-bit integers are `bigint` everywhere.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/node/index.js` | CommonJS loader: tries `./build/Release/weaveffi.node`, falls back to `./index.node`; idiomatic wrappers |
| `generated/node/types.d.ts` | TypeScript declarations for the public surface |
| `generated/node/weaveffi_addon.c` | N-API addon source: marshaling, promises, callback vtables, threadsafe functions |
| `generated/node/binding.gyp` | node-gyp build file (includes `../c`, links `-lweaveffi`) |
| `generated/node/package.json` | npm package metadata (`main`, `types`, `gypfile`, install script) |

## Type mapping

| IDL type      | TypeScript type      | Notes |
|---------------|----------------------|-------|
| `i8`, `i16`, `i32` | `number`        |       |
| `u8`, `u16`, `u32` | `number`        |       |
| `i64`         | `bigint`             | Full 64-bit range; never rounded |
| `u64`         | `bigint`             | Full 64-bit range; never rounded |
| `f32`, `f64`  | `number`             | IEEE values; NaN, infinities, `-0` preserved |
| `bool`        | `boolean`            |       |
| `string`      | `string`             |       |
| `bytes`       | `Buffer`             |       |
| `StructName`  | `StructName` (a plain object interface) | Value buffer |
| `InterfaceName` | `class InterfaceName` | One strong reference per instance; see [Objects](#objects-interfaces) |
| `InterfaceName?` | `InterfaceName \| null` | |
| `CallbackName` | `interface CallbackName` | Any object with the right methods; see [Callback interfaces](#callback-interfaces) |
| `EnumName` (plain, C-style)   | `enum EnumName`  | A frozen object with forward and reverse mappings at runtime |
| `EnumName` (rich / algebraic) | discriminated union type (e.g. `Shape`) | Value buffer |
| `T?`          | `T \| null`          | Value buffer |
| `[T]`         | `T[]`                | Value buffer |
| `{K: V}`      | `Record<K, V>`       | Value buffer |
| `iter<T>`     | `IterableIterator<T>` (lazy) | |

`i64` and `u64` are `bigint` in every position: parameters, returns,
record fields, and callback arguments and results. Passing a `number`
where a `bigint` is expected is a type error in the addon; the `codec`
sample declares `roundtripI64(value: bigint): bigint` and
`roundtripU64(value: bigint): bigint`, and the `kvstore` sample's
`Store.count()` returns `1n`, not `1`. Floats cross as raw IEEE values
(`writeDoubleLE`/`readDoubleLE` in the value-buffer codec), so NaN, the
infinities, and `-0` round-trip.

## Example IDL → generated code

```yaml
version: "0.9.0"
modules:
  - name: contacts
    enums:
      - name: Color
        variants:
          - { name: Red, value: 0 }
          - { name: Green, value: 1 }
          - { name: Blue, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: tags, type: "[string]" }

    functions:
      - name: get_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: set_favorite_color
        params:
          - { name: contact_id, type: i32 }
          - { name: color, type: "Color?" }
```

Structs become TypeScript interfaces backed by plain JS objects (the
loader packs and unpacks them from value buffers with generated private
codec functions; there are no per-struct native symbols), and enums
become explicit numeric TypeScript enums:

```typescript
export interface Contact {
  name: string;
  email: string | null;
  tags: string[];
}

export enum Color {
  Red = 0,
  Green = 1,
  Blue = 2,
}
```

Functions are exported flat in lowerCamelCase with the module prefix
stripped by default (`strip_module_prefix = false` in `[generators.node]`
restores `<module>_`-prefixed names); parameters are camelCased too, while
record field names keep their IDL spelling (`created_at`). Optional
return and parameter types use `| null`, arrays use `T[]`:

```typescript
export function getContact(id: number): Contact | null
export function listContacts(): Contact[]
export function setFavoriteColor(contactId: number, color: Color | null): void
```

## Typed errors

Every generated `index.js` exports `WeaveFFIError` (extending `Error`
with a numeric `code` and the raw `errorMessage`). A module's error
domain adds a class named after the domain plus one subclass per code,
each carrying its stable `CODE`. From the `kvstore` sample:

```js
class WeaveFFIError extends Error {
  constructor(code, message) {
    super('(' + code + ') ' + (message || ''));
    this.name = 'WeaveFFIError';
    this.code = code;
    this.errorMessage = message || '';
  }
}

class KvError extends WeaveFFIError { /* ... */ }

class KeyNotFoundError extends KvError {
  constructor(message) {
    super(1001, message || 'key not found');
    this.name = 'KeyNotFoundError';
  }
}
KeyNotFoundError.CODE = 1001;
// ExpiredError, StoreFullError, IoError follow the same shape.

const __kvErrorCodes = Object.freeze({ 1001: KeyNotFoundError, 1002: ExpiredError, 1003: StoreFullError, 1004: IoError });
function __kvErrorFrom(code, message, payload) {
  const _cls = __kvErrorCodes[code];
  return _cls === undefined ? new WeaveFFIError(code, message) : new _cls(message);
}
```

A callable with `throws: true` rebrands any native failure through the
domain's code map, so consumers catch the typed class:

```js
try {
  store.put('alpha', Buffer.from('1'), EntryKind.Volatile, null);
} catch (e) {
  if (e instanceof StoreFullError) {
    // typed case; e.code === 1003
  } else if (e instanceof KvError) {
    // any kv domain error
  } else if (e instanceof WeaveFFIError) {
    // runtime code (negative)
  }
}
```

A callable without `throws` has the same JS signature and rebrands
through `__generic`, so every failure is a plain `WeaveFFIError`. Unknown
codes on the typed path fall back to `WeaveFFIError` as well.

### Runtime error codes

| Code | ABI name | When |
|------|----------|------|
| -1 | `GENERIC_ERROR_CODE` | The producer reported a failure with no domain code |
| -2 | `PANIC_ERROR_CODE` | The Rust producer panicked inside an export or a spawned async future; also used by the loader for a malformed value buffer |
| -3 | `MARSHAL_ERROR_CODE` | A null object or a malformed value at the boundary; also thrown by the loader for use after `close()`, a wrong wrapper class, or a null callback implementation |
| -4 | `FOREIGN_ERROR_CODE` | A callback-interface implementation threw, is missing a method, or returned a value of the wrong type |

They surface as a thrown `WeaveFFIError` from a sync call, as a rejected
`Promise` from an async one, and from the `next()` step of an iterator.
JavaScript has no trap path: a non-throwing callable throws the generic
class the same way.

## Objects (interfaces)

An `interfaces:` entry becomes a JS class holding one strong reference to
a reference-counted native object. A constructor named `new` becomes the
class constructor (`new EventBus()` in the `events` sample); other
constructors become static factories; methods are instance methods and
statics are static methods, all camelCased. From the `kvstore` sample's
`Store` (trimmed from `index.js`):

```js
// Borrow a live object wrapper's native handle for one call. The wrapper
// keeps its own reference; the producer clones if it retains the object.
function __borrow(o, cls) {
  if (!(o instanceof cls)) {
    throw new WeaveFFIError(-3, 'expected an instance of ' + cls.name);
  }
  if (!o._handle) {
    throw new WeaveFFIError(-3, cls.name + ' used after close()');
  }
  return o._handle;
}

class Store {
  static open(path) {
    const _r = __invoke(addon.Store_open, [path], __kvErrorFrom);
    return Store._adopt(_r);
  }
  put(key, value, kind, ttlSeconds) {
    return __invoke(addon.Store_put, [__borrow(this, Store), key, value, kind, __encode((w, v) => __wOpt(w, v, (w, v) => w.i64(v)), ttlSeconds)], __kvErrorFrom);
  }
  count() {
    return __invoke(addon.Store_count, [__borrow(this, Store)], __generic);
  }
  compact() {
    return __invokeAsync(addon.Store_compact, [__borrow(this, Store)], __kvErrorFrom);
  }
  static defaultCapacity() {
    return __invoke(addon.Store_default_capacity, [], __generic);
  }
  close() {
    if (this._handle) {
      Store._cleanup.unregister(this);
      addon.Store__destroy(this._handle);
      this._handle = 0n;
    }
  }
  _cloneHandle() {
    return addon.Store__clone(__borrow(this, Store));
  }
}
Store._adopt = function (handle) {
  const _o = Object.create(Store.prototype);
  _o._handle = handle;
  Store._cleanup.register(_o, handle, _o);
  return _o;
};
Store._cleanup = new FinalizationRegistry((handle) => {
  if (handle) { addon.Store__destroy(handle); }
});
if (typeof Symbol.dispose === 'symbol') {
  Store.prototype[Symbol.dispose] = Store.prototype.close;
}
```

The typed declarations mirror the class, with `@throws` and
`@deprecated` JSDoc tags:

```typescript
export class Store {
  /** @throws {KvError} */
  static open(path: string): Store;
  /** @throws {KvError} */
  put(key: string, value: Buffer, kind: EntryKind, ttlSeconds: bigint | null): boolean;
  count(): bigint;
  /** @throws {KvError} */
  compact(): Promise<bigint>;
  static defaultCapacity(): bigint;
  /**
   * Release this wrapper's reference to the native object. Safe to call
   * more than once; a wrapper that is never closed is released when it is
   * garbage collected. Using the wrapper after `close()` throws.
   */
  close(): void;
  /** Alias of `close()` for `using` declarations. */
  [Symbol.dispose](): void;
}
```

- **Disposal is `close()` plus a `FinalizationRegistry` backstop.**
  `close()` releases the wrapper's reference exactly once and is
  idempotent; `Symbol.dispose` aliases it so `using store = Store.open(...)`
  works where the runtime supports explicit resource management. A
  wrapper collected unclosed is released by the registry, so `_destroy`
  runs exactly once either way.
- **Use after close throws** `WeaveFFIError` with code -3 (`Store used
  after close()`), from `__borrow`, before anything reaches the addon.
- **Clones mint a new strong reference.** `_cloneHandle()` calls
  `_clone` whenever the wrapper must hand the producer a reference it
  will own; every adopted wrapper (a return, an async result, an iterated
  element, a buffer token) owes its own `close()`. Two wrappers over the
  same object (`share()`, or the bus handed to `onAttached`) are
  independent: closing one leaves the other usable.

### Objects as parameters, returns, and inside values

A top-level object parameter is borrowed (`__borrow`); a returned object
is adopted. `Store?` is `Store | null` both ways:

```js
larger(other) {
  const _r = __invoke(addon.Store_larger, [__borrow(this, Store), (other == null ? null : __borrow(other, Store))], __generic);
  return (_r == null ? null : Store._adopt(_r));
}
```

Objects inside records, lists, map values, optionals, and rich-enum
payloads are ordinary properties (`store: Store; mirror: Store | null` in
`StoreInfo`). On the wire they're `u64` tokens: the pack function writes a
fresh `_cloneHandle()` per object and the unpack function adopts:

```js
function __packStoreInfo(w, v) {
  w.str(v.label);
  w.u64(v.store._cloneHandle());
  __wOpt(w, v.mirror, (w, v) => w.u64(v._cloneHandle()));
  w.i64(v.count);
}
function __unpackStoreInfo(r) {
  return {
    label: r.str(),
    store: Store._adopt(r.u64()),
    mirror: __rOpt(r, (r) => Store._adopt(r.u64())),
    count: r.i64(),
  };
}
```

`Store.openMany(paths)` returns `Store[]` (one adopted wrapper each);
`Store.totalCount(stores, extra)` clones every object it encodes. An
async callable returning an object resolves with an adopted wrapper, and
an `iter<Interface>` adopts one per step.

## Callback interfaces

A `callback_interfaces:` entry becomes a TypeScript `interface`. Any
object with the right methods satisfies it: a class instance, an object
literal, or a module. From the `events` sample:

```typescript
export interface Subscriber {
  route(topic: string): Delivery;
  onMessage(message: Message): bigint;
  onAttached(bus: EventBus): void;
}
```

```js
const subscriber = {
  route(topic) { return topic === 'quiet' ? Delivery.Skip : Delivery.Accept; },
  onMessage(message) { console.log(message.topic, message.text); return 1n; },
  onAttached(bus) { this.bus = bus; },   // or bus.close() to release it now
};
const bus = new wv.EventBus();
bus.subscribe(subscriber);
```

The loader wraps your object in a small adapter that converts arguments
(decoding buffered values, adopting objects) and maps camelCase method
names to the ABI's snake_case:

```js
function __adaptSubscriber(impl) {
  if (impl === null || impl === undefined) {
    throw new WeaveFFIError(-3, 'Subscriber implementation must be an object');
  }
  return {
    route(topic) {
      return impl.route(topic);
    },
    on_message(message) {
      return impl.onMessage(__decode(__unpackMessage, message));
    },
    on_attached(bus) {
      return impl.onAttached(EventBus._adopt(bus));
    },
  };
}
```

The addon keeps the adapter behind a `napi_ref`, installs one static
vtable per interface, and gives each implementation its own
`napi_threadsafe_function`. A trampoline calls the implementation
directly when the producer is on the JS thread, and otherwise hops the
call to the JS thread and blocks until it has run:

```c
static weaveffi_events_Delivery weaveffi_events_Subscriber_route_tramp(void* ctx, const char* topic, weaveffi_error* out_err) {
  weaveffi_events_Subscriber_route_frame f;
  memset(&f, 0, sizeof f);
  f.hdr.out_err = out_err;
  f.topic = topic;
  weaveffi_napi_cb_ctx* c = (weaveffi_napi_cb_ctx*)ctx;
  if (weaveffi_napi_on_js_thread()) {
    weaveffi_events_Subscriber_route_invoke(c->env, c, &f);
  } else {
    weaveffi_napi_cb_req req;
    req.ctx = c;
    req.method = 0;
    req.frame = &f;
    weaveffi_napi_cb_hop(&req);   // napi_call_threadsafe_function + wait on a condvar
  }
  return f.result;
}
```

- **Argument ownership.** Strings and buffered values are decoded into
  fresh JS values. An object argument (`bus`) is adopted into a wrapper
  your implementation owns: keep it, or `close()` it; the finalizer
  backstops it otherwise.
- **Lifetime.** The `napi_ref` keeps the implementation alive for as long
  as the producer holds it; the vtable's `free` releases the reference and
  the threadsafe function (hopping to the JS thread if needed). The
  threadsafe function is unref'd, so a live implementation doesn't by
  itself keep the event loop alive.
- **Errors.** A thrown exception, a missing method, or a return value of
  the wrong type (for example a `number` where a `bigint` is required)
  never unwinds through the C frame. The addon clears the pending
  exception, takes its `message`, and reports
  `weaveffi_error_set(out_err, -4, ...)`. The producer aborts the call
  that triggered the callback and the caller sees `WeaveFFIError` with
  `code === -4`, thrown from a sync call or rejecting the `Promise` of an
  async one. The implementation stays attached.
- **Threads.** Callback methods always run on the JS thread. When the
  producer calls from one of its own threads (the `events` sample's
  `publishLater`), that thread blocks until the JS thread has run the
  method, which requires the event loop to be free. A synchronous call
  from JS that waits on a producer worker which in turn needs a callback
  will deadlock, since the JS thread is busy in the synchronous call. Use
  async callables for any path that invokes callbacks from a worker.

## Rich (algebraic) enums

A *rich* (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style enum stays a numeric TypeScript `enum`, but a rich
enum crosses the ABI as a serialized value buffer (`i32` tag, then the
active variant's fields) and surfaces in JS as a plain **tagged-union
object** with a string `tag` property naming the active variant. No
native handles, no classes, no destructors.

Take a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`. The generated `types.d.ts`
declares a discriminated union:

```typescript
export type Shape =
  | { tag: 'Empty' }
  | { tag: 'Circle'; radius: number }
  | { tag: 'Rectangle'; width: number; height: number }
  | { tag: 'Labeled'; label: string; count: number };
```

The loader carries one private pack and one unpack function per rich
enum: packing switches on the string `tag`, writes the `i32`
discriminant, then the variant's fields; unpacking reads the tag and
builds the matching object. Consumers construct and match variants as
ordinary JS values:

```js
const { describe, scale } = require('./index.js');

const circle = { tag: 'Circle', radius: 2.0 };
console.log(describe(circle));
const bigger = scale(circle, 3.0);
if (bigger.tag === 'Circle') {
  console.log(bigger.radius);       // 6
}
```

There is nothing to release: values are copied across the boundary, and
the loader frees each returned native buffer immediately after decoding
it. A variant payload that holds an object follows the token rules above.

## Build instructions

The generated addon is self-contained: run `npm install` (the install
script runs `node-gyp rebuild` on the generated `binding.gyp`) inside
`generated/node/` with the generated C headers at `../c` and the
producer cdylib on the linker path:

```bash
cargo build -p kvstore
weaveffi generate samples/kvstore/src/lib.rs -o generated --target c,node

cd generated/node
npm install          # builds build/Release/weaveffi.node
DYLD_LIBRARY_PATH=../../target/debug node -e "
  const kv = require('./index.js');
  const store = kv.Store.open('/tmp/cache.kv');
  console.log(store.count());
  store.close();
"
```

(Use `LD_LIBRARY_PATH` on Linux.) Copying a prebuilt platform binary in
as `index.node` also works, and the `WEAVEFFI_ADDON` env var can point
the loader at any built addon (the `conformance/node/` consumers use it;
see `conformance/run.sh`).

## Packaging

`weaveffi package --target node` emits the main npm package (`index.js`,
`types.d.ts`, `weaveffi_addon.c`, `binding.gyp`, `package.json`) plus one
platform package per desktop slice under `node/npm/<pkg>-<os>-<cpu>/`
(`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`,
`win32-x64`), each gated by npm `os`/`cpu` fields and bundling its
prebuilt producer library. The main package lists them as
`optionalDependencies`, so npm installs only the one matching the host.
Android and `wasm32` binaries have no npm tokens and are skipped. See
[Packaging and Distribution](../guides/packaging.md).

## Memory and ownership

- The N-API addon is responsible for all conversions between JS values
  and C ABI types. Strings and byte buffers are copied into JS-managed
  storage, so consumers never need to think about freeing memory.
- Buffered values (structs, rich enums, optionals, lists, maps) are
  returned as plain JS values: the loader decodes the returned value
  buffer, which the addon has already freed with the native free. Object
  fields inside are cloned on the way in and adopted on the way out.
- Object wrappers own one strong reference; release it with `close()`
  (or `using`); the `FinalizationRegistry` backstops forgotten wrappers
  at GC time.
- `iter<T>` returns are lazy JS iterables; see
  [Iterators](#iterators). The native handle is released on
  exhaustion, on early exit, or by a finalizer as a backstop.
- Errors from the C ABI are converted into JavaScript `Error` instances
  by the addon, then rebranded into the typed error classes by the
  loader before bubbling up to the caller.

## Async support

Async IDL functions return a `Promise`:

```typescript
compact(): Promise<bigint>;
```

The addon creates the promise with `napi_create_promise` and calls the
C ABI `_async` entry point, which runs the work on a native producer
thread. The promise is never settled from that thread: the completion
callback only stashes the result (or error) and posts it through a
`napi_threadsafe_function` whose settle callback runs on the JS event
loop and calls `napi_resolve_deferred` / `napi_reject_deferred` there:

```c
static void weaveffi_events_EventBus_publish_later_napi_cb(void* context, weaveffi_error* err, int64_t result) {
    weaveffi_events_EventBus_publish_later_napi_actx* ctx = (weaveffi_events_EventBus_publish_later_napi_actx*)context;
    if (err != NULL && err->code != 0) {
        ctx->err_code = err->code;
        ctx->err_msg = err->message ? strdup(err->message) : strdup("unknown error");
        /* ... copy the payload buffer the same way ... */
    } else {
        ctx->result = result;
    }
    weaveffi_error_free(err);
    napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking);
}
```

The completion callback fires exactly once, on the producer thread.
Result buffers passed to it (strings, byte buffers, and value buffers)
are owned by the consumer, so the callback deep-copies them and then
releases them with `weaveffi_free_string` or `weaveffi_free_bytes`; a
reported error is heap-boxed and released with `weaveffi_error_free`
after its fields are copied. An object result is adopted into a wrapper
by the loader when the promise resolves.

Rejected promises carry a numeric `code`; the loader rebrands the
rejection into the module's typed error class when the callable declares
`throws: true` (`Store.compact()` rejects with `KvError` subclasses), and
into the generic `WeaveFFIError` otherwise. A pending async call keeps
the event loop alive until it settles.

For functions marked `cancellable: true` the addon passes `NULL` for
the C ABI's cancel-token slot; the token is not surfaced to JS and
there is no `AbortSignal` parameter.

## Iterators

`iter<T>` returns are lazy: the addon hands back an opaque external
wrapping the native iterator handle, and the loader wraps it in a
shared `WeaveFFIIterator` class implementing the JS iterator protocol.
Nothing is drained up front; each `next()` issues exactly one native
`_iterNext` call:

```js
class WeaveFFIIterator {
  next() {
    if (this._done) {
      return { done: true, value: undefined };
    }
    const _v = __invoke(this._nextFn, [this._ext], this._map);
    if (_v === undefined) {
      this._done = true;
      return { done: true, value: undefined };
    }
    return { done: false, value: this._wrapElem ? this._wrapElem(_v) : _v };
  }
  return(value) {
    if (!this._done) {
      this._done = true;
      this._destroyFn(this._ext);
    }
    return { done: true, value };
  }
  [Symbol.iterator]() {
    return this;
  }
}

listKeys(prefix) {
  const _it = __invoke(addon.Store_list_keys, [__borrow(this, Store), __encode((w, v) => __wOpt(w, v, (w, v) => w.str(v)), prefix)], __kvErrorFrom);
  return new WeaveFFIIterator(_it, addon.Store_list_keys_iterNext, addon.Store_list_keys_iterDestroy, __kvErrorFrom, null);
}
```

The TypeScript declaration is `IterableIterator<T>`, so a plain
`for...of` loop (or `[...store.listKeys(null)]`) works, and `break`ing
out of the loop triggers `return()`, which destroys the native iterator
early. The addon's `_iterNext` binding pulls one element, copies it into
a JS value (freeing the native string or buffer), and destroys the
iterator eagerly when the producer reports exhaustion; the external's
N-API finalizer backstops abandoned iterators at GC time. Buffered
elements are decoded per step; object elements are adopted through
`_wrapElem`.

Errors from the launcher and each `next` step follow the function's
error strategy: `Store.listKeys` (throws) rebrands a failing step
through `__kvErrorFrom` into the typed `KvError` subclasses, while a
non-throwing function throws the generic `WeaveFFIError`.

## Known limitations

- Callback methods run only on the JS thread. A producer thread that
  invokes one blocks until the event loop services it, so a synchronous
  call that waits on such a thread deadlocks; prefer async callables for
  callback-heavy paths.
- `i64`/`u64` require `bigint`; a `number` in those positions is rejected
  at the boundary.
- Cancellation tokens aren't exposed; there is no `AbortSignal` support.
- `Symbol.dispose` is only wired when the runtime defines it.
- The loader is CommonJS; import it from ESM with `createRequire` or a
  default import.

## Troubleshooting

- **`Error: Cannot find module './index.node'`**: no addon binary was
  found at either loader path. Run `npm install` in `generated/node/`
  to build the generated addon with node-gyp, or copy a prebuilt
  binary in as `index.node`.
- **`dlopen: ... image not found`**: the addon links against the
  Rust cdylib at runtime; set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the cdylib next to `index.node`.
- **`WeaveFFIError: (-3) Store used after close()`**: the wrapper was
  closed (or disposed by `using`) before the call. Adopt a second wrapper
  if you need a longer-lived reference.
- **`WeaveFFIError: (-4) ...`**: your callback implementation threw, is
  missing a method, or returned the wrong type; check that `bigint`
  results are returned as `1n`, not `1`.
- **Process hangs during a sync call**: a callback is being invoked from a
  producer thread while the JS thread is blocked in that call. Switch the
  call to an async callable.
- **TypeScript complains about missing types**: point `tsconfig`'s
  `paths` at `generated/node/types.d.ts` or include the generated
  package in `compilerOptions.types`.
