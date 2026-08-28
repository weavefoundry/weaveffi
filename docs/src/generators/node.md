# Node.js

## Overview

The Node.js target produces a CommonJS loader, TypeScript type
definitions, and the complete N-API addon C source (plus a
`binding.gyp`) that bridges JS to the C ABI. The loader honors a
`WEAVEFFI_ADDON` environment override, then prefers the node-gyp build
output (`./build/Release/weaveffi.node`), and falls back to a prebuilt
binary placed next to it as `index.node`. On top of the raw native
bindings it layers the idiomatic wrappers: error classes, interface
classes, plain-object records and tagged-union rich enums, and
camelCased function wrappers.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/node/index.js` | CommonJS loader: tries `./build/Release/weaveffi.node`, falls back to `./index.node` |
| `generated/node/types.d.ts` | TypeScript declarations for the public surface |
| `generated/node/weaveffi_addon.c` | N-API addon source: marshaling, promises, threadsafe functions |
| `generated/node/binding.gyp` | node-gyp build file (includes `../c`, links `-lweaveffi`) |
| `generated/node/package.json` | npm package metadata (`main`, `types`, `gypfile`, install script) |

## Type mapping

| IDL type      | TypeScript type      |
|---------------|----------------------|
| `i32`         | `number`             |
| `u32`         | `number`             |
| `i8`          | `number`             |
| `i16`         | `number`             |
| `u8`          | `number`             |
| `u16`         | `number`             |
| `i64`         | `number`             |
| `u64`         | `number`             |
| `f64`         | `number`             |
| `f32`         | `number`             |
| `bool`        | `boolean`            |
| `string`      | `string`             |
| `bytes`       | `Buffer`             |
| `handle`      | `bigint`             |
| `StructName`  | `StructName` (a plain object interface) |
| `EnumName` (plain, C-style)   | `enum EnumName`                |
| `EnumName` (rich / algebraic) | discriminated union type (e.g. `Shape`) |
| `T?`          | `T \| null`          |
| `[T]`         | `T[]`                |
| `{K: V}`      | `Record<K, V>`       |
| `iter<T>`     | `IterableIterator<T>` (lazy) |

## Example IDL → generated code

```yaml
version: "0.7.0"
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

      - name: get_tags
        params:
          - { name: contact_id, type: i32 }
        return: "[string]"
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
stripped by default (`strip_module_prefix = false` in `[node]` restores
`<module>_`-prefixed names); parameters are camelCased too. Optional
return and parameter types use `| null`, arrays use `T[]`:

```typescript
export function getContact(id: number): Contact | null
export function listContacts(): Contact[]
export function setFavoriteColor(contactId: number, color: Color | null): void
export function getTags(contactId: number): string[]
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
  }
}
```

A callable without `throws` has the same JS signature (JavaScript has no
checked exceptions), but its failures can only be producer bugs, which
surface as the generic `WeaveFFIError`. Unknown codes on the typed path
fall back to `WeaveFFIError` as well.

## Interfaces

An `interfaces:` entry becomes a JS class owning the native pointer,
registered with a `FinalizationRegistry` and freed deterministically via
`destroy()`. Constructors become static factories, methods are instance
methods, statics are static methods, all camelCased. From the `kvstore`
sample's `Store` (trimmed from `index.js`):

```js
class Store {
  static open(path) {
    const _r = __invoke(addon.Store_open, [path], __kvErrorFrom);
    return Store._fromHandle(_r);
  }
  put(key, value, kind, ttlSeconds) {
    return __invoke(addon.Store_put, [this._handle, key, value, kind, ttlSeconds], __kvErrorFrom);
  }
  listKeys(prefix) {
    const _it = __invoke(addon.Store_list_keys, [this._handle, prefix], __kvErrorFrom);
    return new WeaveFFIIterator(_it, addon.Store_list_keys_iterNext, addon.Store_list_keys_iterDestroy, __kvErrorFrom, null);
  }
  count() {
    return __invoke(addon.Store_count, [this._handle], __generic);
  }
  compact() {
    return __invokeAsync(addon.Store_compact, [this._handle], __kvErrorFrom);
  }
  static defaultCapacity() {
    return __invoke(addon.Store_default_capacity, [], __generic);
  }
  destroy() {
    if (this._handle) {
      Store._cleanup.unregister(this);
      addon.Store_destroy(this._handle);
      this._handle = 0;
    }
  }
}
Store._cleanup = new FinalizationRegistry((handle) => {
  if (handle) { addon.Store_destroy(handle); }
});
```

The typed declarations mirror the class, with `@throws` and
`@deprecated` JSDoc tags:

```typescript
export class Store {
  /** @throws {KvError} */
  static open(path: string): Store;
  /** @throws {KvError} */
  put(key: string, value: Buffer, kind: EntryKind, ttlSeconds: number | null): boolean;
  /** @throws {KvError} */
  listKeys(prefix: string | null): IterableIterator<string>;
  count(): number;
  /** @throws {KvError} */
  compact(): Promise<number>;
  static defaultCapacity(): number;
  /** Free the underlying native object. */
  destroy(): void;
}
```

A function elsewhere in the API that takes the interface accepts the
wrapper instance and unwraps its handle (`getStats(store)` in the nested
`stats` module); a function returning an interface wraps the new owned
handle in a fresh instance. Call `destroy()` when you're done; the
`FinalizationRegistry` is only a GC-timed safety net.

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
const labeled = { tag: 'Labeled', label: 'unit', count: 3 };

console.log(describe(circle));      // native-rendered description
const bigger = scale(circle, 3.0);  // a fresh Shape value

if (bigger.tag === 'Circle') {
  console.log(bigger.radius);       // 6
}
```

There is nothing to release: values are copied across the boundary, and
the loader frees each returned native buffer immediately after decoding
it.

## Build instructions

The generated addon is self-contained: run `npm install` (the install
script runs `node-gyp rebuild` on the generated `binding.gyp`) inside
`generated/node/` with the generated C headers at `../c` and the
producer cdylib on the linker path:

```bash
cargo build -p kvstore
weaveffi generate samples/kvstore/kvstore.yml -o generated

cd generated/node
npm install          # builds build/Release/weaveffi.node
DYLD_LIBRARY_PATH=../../target/debug node -e "
  const kv = require('./index.js');
  const store = kv.Store.open('/tmp/cache.kv');
  console.log(store.count());
"
```

(Use `LD_LIBRARY_PATH` on Linux.) Then publish the generated directory
as a private npm package or ship it inside your app. Copying a prebuilt
platform binary in as `index.node` also works, and the `WEAVEFFI_ADDON`
env var can point the loader at any built addon (the
`conformance/node/` consumers use it; see `conformance/run.sh`).

## Memory and ownership

- The N-API addon is responsible for all conversions between JS values
  and C ABI types. Strings and byte buffers are copied into JS-managed
  storage, so consumers never need to think about freeing memory.
- Buffered values (structs, rich enums, optionals, lists, maps) are
  returned as plain JS values: the loader decodes the returned value
  buffer and frees it with the native free before the call returns, so
  there is nothing to dispose on the JS side.
- Interface wrappers own their native pointer; release it with
  `destroy()` (a `FinalizationRegistry` backstops forgotten handles at
  GC time).
- Typed handles (`handle<Struct>`) pass through as opaque values;
  release them through the API's own teardown function.
- `iter<T>` returns are lazy JS iterables; see
  [Iterators](#iterators). The native handle is released on
  exhaustion, on early exit, or by a finalizer as a backstop.
- Errors from the C ABI are converted into JavaScript `Error` instances
  by the addon, then rebranded into the typed error classes by the
  loader before bubbling up to the caller.

## Async support

Async IDL functions are exposed as JS functions that return a Promise:

```typescript
export function runTask(name: string): Promise<TaskResult>
```

The addon creates the promise with `napi_create_promise` and calls the
C ABI `_async` entry point, which runs the work on a native producer
thread. The promise is never settled from that thread: the completion
callback only stashes the result (or error) and posts it through a
`napi_threadsafe_function` whose settle callback runs on the JS event
loop and calls `napi_resolve_deferred` / `napi_reject_deferred` there:

```c
static void weaveffi_tasks_run_task_napi_cb(void* context, weaveffi_error* err,
                                            const uint8_t* result_ptr, size_t result_len) {
    weaveffi_tasks_run_task_napi_actx* ctx = (weaveffi_tasks_run_task_napi_actx*)context;
    if (err != NULL && err->code != 0) {
        ctx->err_code = err->code;
        ctx->err_msg = err->message ? strdup(err->message) : strdup("unknown error");
    } else {
        ctx->result_len = result_len;
        if (result_ptr != NULL && result_len > 0) {
            ctx->result = (uint8_t*)malloc(result_len);
            memcpy(ctx->result, result_ptr, result_len);
        }
        weaveffi_free_bytes((uint8_t*)result_ptr, result_len);
    }
    weaveffi_error_free(err);
    napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking);
}
```

The completion callback fires exactly once, on the producer thread.
Result buffers passed to it (strings, byte buffers, and the serialized
value buffers of buffered results such as the `TaskResult` record here)
are owned by the consumer, so the callback deep-copies them (note the
`strdup` and `memcpy` above) and then releases them with
`weaveffi_free_string` or `weaveffi_free_bytes`; a reported error is
heap-boxed and released with `weaveffi_error_free` after its fields
are copied. Owned interface results transfer ownership too: the
callback receives the object pointer, which the settle callback wraps
into the JS-side owner.

Rejected promises carry the C error message plus a numeric `code`
property; the loader rebrands the rejection into the module's typed
error class when the callable declares `throws: true` (an async method
like `Store.compact()` rejects with `KvError` subclasses), and into the
generic `WeaveFFIError` otherwise. The settle callback releases the
threadsafe function once the promise is settled, so a pending async
call keeps the event loop alive until it completes.

For functions marked `cancellable: true` the addon passes `NULL` for
the C ABI's cancel-token slot; the token is not surfaced to JS and
there is no `AbortSignal` parameter. Only the C, C++, and Kotlin
targets expose cancellation tokens.

## Iterators

`iter<T>` returns are lazy: the addon hands back an opaque external
wrapping the native iterator handle, and the loader wraps it in a
shared `WeaveFFIIterator` class implementing the JS iterator protocol.
Nothing is drained up front; each `next()` issues exactly one native
`_iterNext` call. From the `events` sample's `index.js`:

```js
// Lazy iterator over a native producer: one native `next` per step.
// The native handle is released on exhaustion, by `return()` on early
// exit, or by the external's finalizer if the iterator is abandoned.
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

wv.getMessages = function () {
  const _it = __invoke(addon.getMessages, [], __generic);
  return new WeaveFFIIterator(_it, addon.getMessages_iterNext, addon.getMessages_iterDestroy, __generic, null);
};
```

The TypeScript declaration is `IterableIterator<T>`, so a plain
`for...of` loop works and `break`ing out of it triggers `return()`,
which destroys the native iterator early. The addon's `_iterNext`
binding pulls one element, copies it into a JS value (freeing the
native string), and destroys the iterator eagerly when the producer
reports exhaustion; the external's N-API finalizer backstops abandoned
iterators at GC time, nulling the stored handle so a double destroy is
impossible. Buffered elements (structs, rich enums, composites) arrive
as producer-allocated value buffers: the addon decodes each one into a
plain JS value and frees the buffer per step.

Errors from the launcher and each `next` step follow the function's
error strategy: `Store.listKeys` (throws) rebrands a failing step
through `__kvErrorFrom` into the typed `KvError` subclasses, while
the non-throwing `getMessages` throws the generic `WeaveFFIError`
only for producer bugs.

## Callbacks and listeners

An IDL `listener` becomes a register/unregister pair. Registration
takes a plain JS function and returns a numeric subscription id;
unregistration takes that id back:

```typescript
export function registerMessageListener(callback: (message: string) => void): number
export function unregisterMessageListener(id: number): void
```

The id is the `uint64` returned by the C ABI's
`weaveffi_events_register_message_listener(callback_fn, context)`; each
registration gets its own id and threadsafe function.

The native callback fires on the producer's thread, and the addon never
calls into JS from there. Registration wraps the JS function in a
`napi_threadsafe_function`, and a C trampoline copies the payload and
queues it onto the JS event loop:

```c
static void weaveffi_events_OnMessage_fn_napi_tramp(const char* message, void* context) {
    weaveffi_napi_listener_ctx* ctx = (weaveffi_napi_listener_ctx*)context;
    weaveffi_events_OnMessage_fn_payload* p = (weaveffi_events_OnMessage_fn_payload*)calloc(1, sizeof(weaveffi_events_OnMessage_fn_payload));
    p->message = message ? strdup(message) : NULL;
    napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking);
}
```

The threadsafe function is unref'd immediately after registration:

```c
napi_create_threadsafe_function(env, args[0], NULL, resource_name, 0, 1, NULL, NULL, NULL, weaveffi_events_OnMessage_fn_napi_calljs, &ctx->tsfn);
napi_unref_threadsafe_function(env, ctx->tsfn);
uint64_t id = weaveffi_events_register_message_listener(weaveffi_events_OnMessage_fn_napi_tramp, ctx);
```

Threading caveats:

- The JS callback always runs on the JS thread; delivery is
  asynchronous and the producer does not wait for it
  (`napi_tsfn_nonblocking`).
- Because the threadsafe function is unref'd, a registered listener
  does not keep the process alive; the loop may exit with listeners
  still registered.
- Unregistering calls the C ABI unregister, releases the threadsafe
  function, and frees the listener context.

## Troubleshooting

- **`Error: Cannot find module './index.node'`**: no addon binary was
  found at either loader path. Run `npm install` in `generated/node/`
  to build the generated addon with node-gyp, or copy a prebuilt
  binary in as `index.node`.
- **`dlopen: ... image not found`**: the addon links against the
  Rust cdylib at runtime; set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the cdylib next to `index.node`.
- **`BigInt` errors with `handle`**: handles are 64-bit; pass them as
  `bigint`, not `number`.
- **TypeScript complains about missing types**: point `tsconfig`'s
  `paths` at `generated/node/types.d.ts` or include the generated
  package in `compilerOptions.types`.
