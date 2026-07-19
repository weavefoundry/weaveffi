# Wasm

## Overview

The Wasm target produces a typed ES module loader for
`wasm32-unknown-unknown` builds of WeaveFFI cdylibs. The loader wraps
the raw exports in idiomatic JavaScript: per-module namespaces, struct
wrapper classes with getters, thrown `Error`s instead of error slots,
`Promise`-based async functions, and automatic string/bytes staging in
linear memory. TypeScript declarations describe the whole surface.

C and C++ producers compiled with Emscripten are supported through a
dedicated loader variant; see [Emscripten mode](#emscripten-mode).

Callbacks and listeners are supported with **synchronous, same-thread
delivery**: a `wasm32-unknown-unknown` module is single-threaded, so
events fire only while a call into the module is on the stack; see
[Callbacks and listeners](#callbacks-and-listeners).

## What gets generated

| File | Purpose |
|------|---------|
| `generated/wasm/weaveffi_wasm.js` | ES module: memory helpers, struct wrapper classes, and the async `loadWeaveffiWasm(url)` loader returning typed bindings |
| `generated/wasm/weaveffi_wasm.d.ts` | TypeScript declarations for the loader and every module namespace |
| `generated/wasm/package.json` | npm package manifest (`type: "module"`) |
| `generated/wasm/README.md` | Quickstart and boundary conventions |

## Type mapping

| IDL type     | Wasm boundary | JavaScript surface |
|--------------|---------------|--------------------|
| `i32` / `u32`| `i32`         | `number`           |
| `i8` / `i16` | `i32`         | `number`           |
| `u8` / `u16` | `i32`         | `number`           |
| `i64`        | `i64`         | `BigInt`           |
| `u64`        | `i64`         | `BigInt`           |
| `f64`        | `f64`         | `number`           |
| `f32`        | `f32`         | `number`           |
| `bool`       | `i32`         | `boolean` (0/1 at the boundary) |
| `string`     | `i32` pointer (NUL-terminated UTF-8) | `string`, staged via `weaveffi_alloc` |
| `bytes`      | `i32` pointer + `i32` length | `Uint8Array` copy |
| `handle` / `StructName` | `i32` pointer into linear memory (0 = null) | struct wrapper class with getters |
| `EnumName` (plain, C-style)   | `i32` discriminant | `number` |
| `EnumName` (rich / algebraic) | `i32` pointer into linear memory (0 = null) | wrapper `class` (e.g. `Shape`) |
| `T?`         | 0 / null pointer; scalars boxed by pointer | `T \| null` |
| `[T]`        | `i32` pointer + `i32` length | `Array` copy |
| `iter<T>`    | iterator handle + `next` out-param | lazy `IterableIterator<T>` |

## Example IDL → generated code

The loader exports a single async entry point that fetches,
instantiates, and wraps a `.wasm` module:

```javascript
import { loadWeaveffiWasm } from './weaveffi_wasm.js';

const api = await loadWeaveffiWasm('/your_library.wasm');
```

Functions are grouped by IDL module in lowerCamelCase (nested IDL
modules nest namespaces, e.g. `api.kv.stats`) and have idiomatic
signatures; strings, arrays, and error handling are taken care of
inside the wrapper:

```javascript
api.events.sendMessage('hello');            // throws WeaveFFIError on failure
for (const m of api.events.getMessages()) { // iter<string> -> lazy iterable
  console.log(m);
}
```

Structs come back as wrapper classes holding the native handle, with a
getter per field and a static `create` when the struct has a
constructor:

```javascript
const result = await api.tasks.runTask('build');
console.log(result.id, result.value, result.success);
```

The raw exports stay reachable for anything not covered by the typed
surface:

```javascript
api._raw.weaveffi_alloc(16);
```

The generated `weaveffi_wasm.d.ts` mirrors all of this for TypeScript
consumers:

```typescript
export interface WeaveffiWasmModule {
  _raw: WebAssembly.Exports;
  events: {
    sendMessage(text: string): void;
    getMessages(): IterableIterator<string>;
  };
}

export function loadWeaveffiWasm(url: string): Promise<WeaveffiWasmModule>;
```

## Typed errors

The module exports `WeaveFFIError` (extending `Error` with a numeric
`code`). A module's error domain adds an exported base class named after
the domain plus one exported class per code, each carrying its stable
`CODE` and reachable both flat and via the domain class. From the
`kvstore` sample:

```js
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
uses the generic checker only; a failure there can only be a producer
bug and throws `WeaveFFIError`.

## Interfaces

An `interfaces:` entry becomes a class exposed on its module's namespace
(`api.kv.Store`). Constructors are static factories, methods are
camelCased instance methods, statics are static methods, and `free()`
releases the native object (there's no `FinalizationRegistry` on this
target). From the `kvstore` sample (trimmed):

```js
// An embedded key-value store owning its entries
class Store {
  free() {
    if (this._handle !== 0) {
      wasm.weaveffi_kv_Store_destroy(this._handle);
      this._handle = 0;
    }
  }
  static open(path) {
    const [a0_p, a0_s] = _cstr(wasm, path);
    const _err = _allocErr(wasm);
    const _r = wasm.weaveffi_kv_Store_open(a0_p, _err);
    wasm.weaveffi_dealloc(a0_p, a0_s);
    _checkKvError(wasm, _err);
    _freeErr(wasm, _err);
    return Store._wrap(_r);
  }
  delete(key) { /* throws typed KvError subclasses */ }
  count() { /* generic check only (no throws) */ }
  compact() {
    return new Promise((resolve, reject) => {
      const ctxId = _nextCtxId++;
      _asyncContexts.set(ctxId, { resolve, reject, mkErr: _kvErrorFrom });
      wasm.weaveffi_kv_Store_compact_async(this._handle, 0, _cbPtr_i32_i32_i64, ctxId);
    });
  }
  /** @deprecated use put() with explicit kind */
  legacyPut(key, value) { /* ... */ }
  static defaultCapacity() { /* ... */ }
}
```

```js
const store = api.kv.Store.open('/tmp/cache.kv');
store.put('alpha', new Uint8Array([1]), api.kv.EntryKind.Volatile, null);
console.log(store.count());
store.free();
```

An interface parameter accepts the wrapper and reads `_handle`
(`api.kv.stats.getStats(store)`); an interface return wraps the owned
pointer in a fresh instance. Call `free()` when done; otherwise the
allocation lives until the module instance is dropped.

## Rich (algebraic) enums

A *rich* (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style enum stays an `i32` discriminant (surfaced as a
`number` plus a frozen constants object), but a rich enum lowers to an
**opaque object handle**, an `i32` pointer into linear memory, exactly
like a struct wrapper. The loader wraps it in a `Shape` class that owns
that handle for the lifetime of the module instance.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`, the generated `Shape` class has
one static factory per variant, a `tag` getter, a getter per variant
field, and an explicit `free()` (there is no `FinalizationRegistry` on
this target):

```js
class Shape {
  constructor(wasm, handle) {
    this._wasm = wasm;
    this._handle = handle;
  }
  get tag() {
    const wasm = this._wasm;
    const _r = wasm.weaveffi_shapes_Shape_tag(this._handle);
    return _r;
  }
  static empty(wasm) {
    const _err = _allocErr(wasm);
    const _r = wasm.weaveffi_shapes_Shape_Empty_new(_err);
    _checkErr(wasm, _err);
    _freeErr(wasm, _err);
    return new Shape(wasm, _r);
  }
  static circle(wasm, radius) {
    const _err = _allocErr(wasm);
    const _r = wasm.weaveffi_shapes_Shape_Circle_new(radius, _err);
    _checkErr(wasm, _err);
    _freeErr(wasm, _err);
    return new Shape(wasm, _r);
  }
  // ... rectangle(wasm, width, height), labeled(wasm, label, count) ...
  get circleRadius() {
    const wasm = this._wasm;
    const _r = wasm.weaveffi_shapes_Shape_Circle_get_radius(this._handle);
    return _r;
  }
  get labeledLabel() {
    const wasm = this._wasm;
    const _r = wasm.weaveffi_shapes_Shape_Labeled_get_label(this._handle);
    return _takeCStr(wasm, _r);
  }
  // ... rectangleWidth, rectangleHeight, labeledCount ...
  free() {
    if (this._handle !== 0) {
      this._wasm.weaveffi_shapes_Shape_destroy(this._handle);
      this._handle = 0;
    }
  }
}
Shape.Tag = Object.freeze({
  Empty: 0,
  Circle: 1,
  Rectangle: 2,
  Labeled: 3,
});
```

The `wasm` instance is bound for you by the loader, so on the returned
API the factories take only their declared arguments. Under
`api.shapes.Shape` you get `empty()`, `circle(radius)`,
`rectangle(width, height)`, `labeled(label, count)`, plus the frozen
`Tag` map:

```js
shapes: {
  // ...
  Shape: {
    empty: (...args) => Shape.empty(wasm, ...args),
    circle: (...args) => Shape.circle(wasm, ...args),
    rectangle: (...args) => Shape.rectangle(wasm, ...args),
    labeled: (...args) => Shape.labeled(wasm, ...args),
    Tag: Shape.Tag,
  },
},
```

The active variant is read through the `tag` getter (no call
parentheses) and compared against `api.shapes.Shape.Tag`. Each variant
field is a camelCased getter: `circleRadius`, `rectangleWidth`,
`rectangleHeight`, `labeledLabel`, `labeledCount`. Functions that take
or return the enum pass the wrapper directly: `describe(shape)` reads
`shape._handle`, and `scale(shape, factor)` returns a fresh `Shape`.

The generated `weaveffi_wasm.d.ts` types the wrapper as an
`export declare class`:

```typescript
export declare class Shape {
  get tag(): number;
  static readonly Tag: Readonly<{
    Empty: 0;
    Circle: 1;
    Rectangle: 2;
    Labeled: 3;
  }>;
  static empty(): Shape;
  static circle(radius: number): Shape;
  static rectangle(width: number, height: number): Shape;
  static labeled(label: string, count: number): Shape;
  get circleRadius(): number;
  get rectangleWidth(): number;
  get rectangleHeight(): number;
  get labeledLabel(): string;
  get labeledCount(): number;
  free(): void;
}
```

A short round-trip that constructs a couple of variants, reads the tag and a
field, calls `describe` / `scale`, then frees the handles:

```js
const api = await loadWeaveffiWasm('/shapes.wasm');

const circle = api.shapes.Shape.circle(2.0);
const label = api.shapes.Shape.labeled('unit', 3);

if (circle.tag === api.shapes.Shape.Tag.Circle) {
  console.log(circle.circleRadius); // 2
}

console.log(api.shapes.describe(circle)); // native-rendered description
const bigger = api.shapes.scale(circle, 3.0); // a fresh Shape

// No FinalizationRegistry on this target. Free handles yourself.
circle.free();
label.free();
bigger.free();
```

**Ownership:** a `Shape` owns its native object. JavaScript has no
deterministic destructors here, so call `free()` when you are done;
otherwise the allocation lives until the module instance is dropped.

## Async support

Async IDL functions return real `Promise`s. The loader grows the
module's `__indirect_function_table` and registers one JavaScript
trampoline per completion-callback signature using the
[JS Type Reflection API](https://github.com/WebAssembly/js-types)
(`new WebAssembly.Function(...)`); each call stores its
`resolve`/`reject` pair in a context map keyed by an integer id:

```javascript
runTask(name) {
  return new Promise((resolve, reject) => {
    const ctxId = _nextCtxId++;
    _asyncContexts.set(ctxId, { resolve, reject, mkErr: _taskErrorFrom, unwrap: (w, h) => new TaskResult(w, h) });
    const [a0_p, a0_s] = _cstr(wasm, name);
    wasm.weaveffi_tasks_run_task_async(a0_p, _cbPtr_i32_i32_i32, ctxId);
    wasm.weaveffi_dealloc(a0_p, a0_s);
  });
}
```

When the producer invokes the completion callback, the trampoline looks
up the context, settles the promise, and removes the entry. A callable
with `throws: true` stores the module's typed error mapper in the
context (`mkErr`), so the rejection carries the domain error; a
non-throwing async callable rejects with the generic `WeaveFFIError`
only when the producer has a bug.

Two caveats apply:

- `WebAssembly.Function` requires a runtime with JS Type Reflection
  (recent V8/SpiderMonkey; Chrome, Firefox, Node 16+, Deno).
- The module is single-threaded: the producer must complete the
  callback on the calling thread (e.g. an executor polled by the same
  thread). A producer that spawns OS threads will not work on
  `wasm32-unknown-unknown`.

A `cancellable` function's ABI symbol takes a `weaveffi_cancel_token*`
parameter; the loader passes a null token, so cancellation isn't
surfaced on this target (`Store.compact()` runs to completion). An IDL
function that models cancellation itself is exposed as a plain function
in the same namespace (e.g. `api.tasks.cancelTask(id)`).

## Iterators

`iter<T>` returns are lazy: the wrapper launches the producer iterator
and hands back a shared `_WeaveFFIIterator` implementing the JS
iterator protocol over the iterator handle. Nothing is drained; each
`next()` issues exactly one producer `next` call through a per-element
slot staged in linear memory. From the `events` sample:

```js
getMessages() {
  const _err = _allocErr(wasm);
  const _it = wasm.weaveffi_events_get_messages(_err);
  _checkErr(wasm, _err);
  _freeErr(wasm, _err);
  return new _WeaveFFIIterator(wasm, _it, 4,
    (it, slot, ep) => wasm.weaveffi_events_GetMessagesIterator_next(it, slot, ep),
    (it) => wasm.weaveffi_events_GetMessagesIterator_destroy(it),
    _checkErr, (w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true)));
}
```

The class settles the handle's lifecycle exactly once: `_close()`
destroys the producer iterator, frees the element slot, and nulls the
handle. It runs eagerly on exhaustion, on a `next` error, or from
`return()` when iteration stops early; a `for...of` loop calls
`return()` automatically on `break` or `throw`. There is no reliable
finalization hook across the runtimes this loader supports, so
abandoning an iterator without exhausting or closing it leaks the
producer handle.

Each decoded element is copied out of linear memory and its producer
allocation released (`_takeCStr` frees strings via
`weaveffi_free_string`). Errors from the launcher and from each `next`
follow the function's error strategy: a throwing function such as the
`kvstore` sample's `Store.listKeys` checks each step with the domain
checker and throws the typed `KvError` subclasses; a non-throwing one
like `getMessages` throws the generic `WeaveFFIError` only for
producer bugs. The TypeScript declaration is `IterableIterator<T>`.

## Callbacks and listeners

Each listener surfaces on its module namespace as a
`register.../unregister...` pair. `register` takes a plain JavaScript
function and returns a numeric subscription id; `unregister` takes that
id and stops delivery. From the `events` sample:

```js
const received = [];
const sub = api.events.registerMessageListener((message) => received.push(message));

api.events.sendMessage('alpha'); // emit_message_listener fires synchronously
console.log(received);           // ['alpha']

api.events.unregisterMessageListener(sub);
```

Under the hood the loader reuses the async machinery: it installs one
long-lived trampoline per callback typedef in the module's
`__indirect_function_table` (via `WebAssembly.Function`, the same
[JS Type Reflection API](https://github.com/WebAssembly/js-types)
dependency async functions have) and hands the trampoline's table index
plus a per-subscription context id to the producer's `register_*`
symbol. When the producer's `emit_*` helper fires, the trampoline looks
up the subscription by context id, decodes each argument, and invokes
the JavaScript callback:

```js
let _nextLsnId = 1;
const _listeners = new Map();

const _lsnPtr_weaveffi_events_OnMessage_fn = _registerTrampoline(_table, ['i32', 'i32'], (a0, _ctx) => {
  const _l = _listeners.get(_ctx);
  if (_l === undefined) return;
  const _p0 = _readCStr(wasm, a0);
  _l.callback(_p0);
});

// On the module object:
registerMessageListener(callback) {
  const _id = _nextLsnId++;
  const _rid = wasm.weaveffi_events_register_message_listener(_lsnPtr_weaveffi_events_OnMessage_fn, _id);
  _listeners.set(_id, { callback, rid: _rid });
  return _id;
},
unregisterMessageListener(id) {
  const _l = _listeners.get(id);
  if (_l === undefined) return;
  _listeners.delete(id);
  wasm.weaveffi_events_unregister_message_listener(_l.rid);
},
```

One trampoline serves every subscription to callbacks of the same
typedef (the context id disambiguates), so register/unregister churn
never grows the function table. The producer's `uint64_t` subscription
id stays internal to the loader; the public surface deals only in plain
numbers.

Two semantic points to keep in mind:

- **Delivery is synchronous and same-thread.** The target is
  single-threaded, so `emit_*` can only run while a call into the
  module is on the stack, and your callback runs before that call
  returns. This is not a limitation of the bindings: a producer that
  emits from a spawned thread cannot run on `wasm32-unknown-unknown`
  at all (`std::thread::spawn` fails there).
- **Callback arguments are borrowed.** The producer owns every argument
  for the duration of the dispatch. Strings and byte buffers are copied
  into JavaScript values before your callback runs, but struct,
  rich-enum, and interface arguments wrap producer-owned memory: read
  what you need inside the callback and do not retain the wrapper or
  call `free()` on it.

In [Emscripten mode](#emscripten-mode) callbacks and listeners are not
supported; each register/unregister entry point becomes an explicit
throwing stub and is omitted from the TypeScript declarations, exactly
like async functions in that mode.

## Emscripten mode

The default loader fetches a bare `.wasm` and calls
`WebAssembly.instantiate` with an empty import object, which only works
for `wasm32-unknown-unknown` builds. A C or C++ library compiled with
Emscripten needs its own JS runtime, its own import object, and exposes
exports as `Module['_name']` rather than `instance.exports.name`. Set
`emscripten` to generate a loader for that layout:

```toml
# weaveffi.toml
[wasm]
emscripten = true
```

or inline in the IDL:

```yaml
generators:
  wasm:
    emscripten: true
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

Async functions, callbacks, and listeners are not supported in
Emscripten mode. The trampoline registration in the standard loader
relies on `WebAssembly.Function` and a growable
`__indirect_function_table`, neither of which an Emscripten module
exposes portably. Each async, register, and unregister entry point
becomes an explicit stub that throws at call time and is omitted from
the TypeScript declarations.

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

## Memory and ownership

- The wrapper stages strings, bytes, and arrays into linear memory with
  the exported `weaveffi_alloc` / `weaveffi_dealloc` and releases them
  after the call; you don't manage buffers for typed calls.
- Producer-owned returns (strings, arrays, struct fields) are copied to
  JavaScript values and freed via `weaveffi_free_string` /
  `weaveffi_dealloc` inside the wrapper.
- Struct wrapper objects hold a native handle. JavaScript has no
  deterministic destructors; the underlying allocation lives until the
  module is dropped. Treat handles as owned by the module instance.
- Error slots are allocated, checked, and cleared internally; failures
  surface as thrown `Error`s with the producer's code and message.
- When you bypass the typed surface via `_raw`, the conventions at the
  top of `weaveffi_wasm.js` apply and every alloc must be paired with a
  dealloc.

## Troubleshooting

- **`WebAssembly.Function is not a constructor`**: the runtime lacks
  JS Type Reflection. Use a current Chrome/Firefox/Node/Deno, or avoid
  async functions, callbacks, and listeners for this target.
- **`LinkError: import object field 'env' is not a Function`**: the
  loader instantiates with an empty imports object. If your Rust crate
  imports host functions, extend `loadWeaveffiWasm` to pass them in.
  If the module was built with Emscripten, use
  [Emscripten mode](#emscripten-mode) instead.
- **An async call never settles**: the producer must invoke the
  completion callback on the same thread; `std::thread::spawn` does not
  exist on `wasm32-unknown-unknown`.
- **A registered listener never fires**: delivery is synchronous, so
  events arrive only while a call into the module is on the stack. The
  producer must `emit_*` during one of your calls; there is no
  background delivery on this target.
- **Out-of-memory after many `_raw` calls**: every pointer returned
  from the module must be deallocated; the typed wrappers do this for
  you, raw calls do not.
- **The `.wasm` file fails to instantiate**: the build artifact must
  be `wasm32-unknown-unknown`. `wasm32-wasi` modules require WASI
  imports and cannot run in the browser without a polyfill.
