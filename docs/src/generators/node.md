# Node.js

## Overview

The Node.js target produces a CommonJS loader, TypeScript type
definitions, and the complete N-API addon C source (plus a
`binding.gyp`) that bridges JS to the C ABI. The loader honors a
`WEAVEFFI_ADDON` environment override, then prefers the node-gyp build
output (`./build/Release/weaveffi.node`), and falls back to a prebuilt
binary placed next to it as `index.node`. On top of the raw native
bindings it layers the idiomatic wrappers: error classes, interface and
rich-enum classes, and camelCased function wrappers.

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
| `StructName`  | `StructName`         |
| `EnumName` (plain, C-style)   | `enum EnumName`                |
| `EnumName` (rich / algebraic) | wrapper `class` (e.g. `Shape`) |
| `T?`          | `T \| null`          |
| `[T]`         | `T[]`                |
| `{K: V}`      | `Record<K, V>`       |
| `iter<T>`     | `T[]` (drained)      |

## Example IDL → generated code

```yaml
version: "0.5.0"
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

Structs become TypeScript interfaces and enums become explicit numeric
TypeScript enums:

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
    return __invoke(addon.Store_list_keys, [this._handle, prefix], __kvErrorFrom);
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
enum lowers to an **opaque object handle** at the C ABI, exactly like a
struct. The loader layers an idiomatic wrapper `class` on top of the raw
native bindings, and that class owns the native pointer.

Take a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`. The generated `index.js` builds
a `Shape` class with one static factory per variant, a `tag()`
discriminant reader, a camelCased getter per variant field, and a
`destroy()` method, backed by a `FinalizationRegistry`:

```js
class Shape {
  static empty() {
    return new Shape(__invoke(addon.Shape_empty_new, [], __generic));
  }
  static circle(radius) {
    return new Shape(__invoke(addon.Shape_circle_new, [radius], __generic));
  }
  static rectangle(width, height) {
    return new Shape(__invoke(addon.Shape_rectangle_new, [width, height], __generic));
  }
  static labeled(label, count) {
    return new Shape(__invoke(addon.Shape_labeled_new, [label, count], __generic));
  }
  tag() {
    return addon.Shape_tag(this._handle);
  }
  get circleRadius() {
    return addon.Shape_circle_get_radius(this._handle);
  }
  get rectangleWidth() {
    return addon.Shape_rectangle_get_width(this._handle);
  }
  get rectangleHeight() {
    return addon.Shape_rectangle_get_height(this._handle);
  }
  get labeledLabel() {
    return addon.Shape_labeled_get_label(this._handle);
  }
  get labeledCount() {
    return addon.Shape_labeled_get_count(this._handle);
  }
  destroy() {
    if (this._handle) {
      Shape._cleanup.unregister(this);
      addon.Shape_destroy(this._handle);
      this._handle = 0;
    }
  }
}
Shape._cleanup = new FinalizationRegistry((handle) => {
  if (handle) { addon.Shape_destroy(handle); }
});
Shape.Tag = Object.freeze({ Empty: 0, Circle: 1, Rectangle: 2, Labeled: 3 });
```

The active variant is read with `tag()` and compared against the frozen
`Shape.Tag` map (`{ Empty: 0, Circle: 1, Rectangle: 2, Labeled: 3 }`).
Each variant field is a getter named `<variant><Field>`
(`circleRadius`, `rectangleWidth`, `rectangleHeight`, `labeledLabel`,
`labeledCount`), delegating to the matching native accessor (e.g.
`addon.Shape_circle_get_radius(this._handle)`). Free functions
that take or return the enum accept the wrapper directly:
`describe(shape)` unwraps `shape._handle`, and
`scale(shape, factor)` wraps its result back into a new `Shape`.

The generated `types.d.ts` types the wrapper as a real `export class`,
with the `Shape.Tag` constants in a companion namespace:

```typescript
export class Shape {
  static empty(): Shape;
  static circle(radius: number): Shape;
  static rectangle(width: number, height: number): Shape;
  static labeled(label: string, count: number): Shape;
  tag(): number;
  get circleRadius(): number;
  get rectangleWidth(): number;
  get rectangleHeight(): number;
  get labeledLabel(): string;
  get labeledCount(): number;
  destroy(): void;
}
export namespace Shape {
  const Tag: Readonly<{
    Empty: 0,
    Circle: 1,
    Rectangle: 2,
    Labeled: 3,
  }>;
}
```

A short round-trip that constructs a couple of variants, reads the tag and a
field, calls `describe` / `scale`, then releases the handles:

```js
const { Shape, describe, scale } = require('./index.js');

const circle = Shape.circle(2.0);
const label = Shape.labeled('unit', 3);

if (circle.tag() === Shape.Tag.Circle) {
  console.log(circle.circleRadius); // 2
}

console.log(describe(circle)); // native-rendered description
const bigger = scale(circle, 3.0); // a fresh Shape

// Done with the handles, release the native objects.
circle.destroy();
label.destroy();
bigger.destroy();
```

**Ownership:** each `Shape` owns its native object. Call `destroy()` when
you are finished to free it deterministically; if you forget, the
`FinalizationRegistry` calls the native destroy once the wrapper is
garbage-collected, but GC timing isn't guaranteed, so prefer an
explicit `destroy()`.

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
- Struct values are returned as plain JS objects: the addon copies the
  fields out and destroys the native struct before the call returns, so
  there is nothing to dispose on the JS side.
- Interface and rich-enum wrappers own their native pointer; release it
  with `destroy()` (a `FinalizationRegistry` backstops forgotten
  handles at GC time).
- Typed handles (`handle<Struct>`) pass through as opaque values;
  release them through the API's own teardown function.
- `iter<T>` returns are drained eagerly inside the addon: it loops the
  C `_next` function into a JS array, frees each native item, and
  destroys the iterator handle before returning.
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
static void weaveffi_tasks_run_task_napi_cb(void* context, weaveffi_error* err, weaveffi_tasks_TaskResult* result) {
    weaveffi_tasks_run_task_napi_actx* ctx = (weaveffi_tasks_run_task_napi_actx*)context;
    if (err != NULL && err->code != 0) {
        ctx->err_code = err->code;
        ctx->err_msg = err->message ? strdup(err->message) : strdup("unknown error");
    } else {
        ctx->result = (void*)result;
    }
    napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking);
}
```

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
