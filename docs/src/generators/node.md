# Node.js

## Overview

The Node.js target produces a CommonJS loader, TypeScript type
definitions, and the complete N-API addon C source (plus a
`binding.gyp`) that bridges JS to the C ABI. The loader prefers the
node-gyp build output (`./build/Release/weaveffi.node`) and falls back
to a prebuilt binary placed next to it as `index.node`
(`samples/node-addon` provides one for the in-tree examples).

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
| `i64`         | `number`             |
| `f64`         | `number`             |
| `bool`        | `boolean`            |
| `string`      | `string`             |
| `bytes`       | `Buffer`             |
| `handle`      | `bigint`             |
| `StructName`  | `StructName`         |
| `EnumName`    | `EnumName`           |
| `T?`          | `T \| null`          |
| `[T]`         | `T[]`                |
| `{K: V}`      | `Record<K, V>`       |
| `iter<T>`     | `T[]` (drained)      |

## Example IDL → generated code

```yaml
version: "0.3.0"
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

Functions are exported with a `<module>_` prefix; optional return and
parameter types use `| null`, arrays use `T[]`:

```typescript
export function contacts_get_contact(id: number): Contact | null
export function contacts_list_contacts(): Contact[]
export function contacts_set_favorite_color(contact_id: number, color: Color | null): void
export function contacts_get_tags(contact_id: number): string[]
```

## Build instructions

The runnable example uses the `calculator` sample.

macOS:

```bash
cargo build -p calculator
cp target/debug/libindex.dylib generated/node/index.node

cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

Linux:

```bash
cargo build -p calculator
cp target/debug/libindex.so generated/node/index.node

cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

Windows: copy `target\debug\index.dll` to `generated\node\index.node`
and run `npm start` from `examples\node`.

For your own project the generated addon is self-contained: run
`npm install` (the install script runs `node-gyp rebuild` on the
generated `binding.gyp`) inside `generated/node/` with the generated C
headers at `../c` and the `weaveffi` cdylib on the linker path. Then
publish the generated directory as a private npm package or ship it
inside your app. Copying a prebuilt platform binary in as `index.node`
(as above) also works.

## Memory and ownership

- The N-API addon is responsible for all conversions between JS values
  and C ABI types. Strings and byte buffers are copied into JS-managed
  storage, so consumers never need to think about freeing memory.
- Struct values are returned as plain JS objects: the addon copies the
  fields out and destroys the native struct before the call returns, so
  there is nothing to dispose on the JS side.
- Typed handles (`handle<Struct>`) pass through as opaque values;
  release them through the API's own teardown function (e.g.
  `kv_close_store`).
- `iter<T>` returns are drained eagerly inside the addon: it loops the
  C `_next` function into a JS array, frees each native item, and
  destroys the iterator handle before returning.
- Errors from the C ABI are converted into JavaScript `Error` instances
  by the addon before bubbling up to the caller.

## Async support

Async IDL functions are exposed as JS functions that return a Promise:

```typescript
export function tasks_run_task(name: string): Promise<TaskResult>
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
property. The settle callback releases the threadsafe function once the
promise is settled, so a pending async call keeps the event loop alive
until it completes.

For functions marked `cancellable: true` the addon passes `NULL` for
the C ABI's cancel-token slot; the token is not surfaced to JS and
there is no `AbortSignal` parameter. Only the C, C++, and Kotlin
targets expose cancellation tokens.

## Callbacks and listeners

An IDL `listener` becomes a register/unregister pair. Registration
takes a plain JS function and returns a numeric subscription id;
unregistration takes that id back:

```typescript
export function events_register_message_listener(callback: (message: string) => void): number
export function events_unregister_message_listener(id: number): void
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

- **`Error: Cannot find module './index.node'`** — no addon binary was
  found at either loader path. Run `npm install` in `generated/node/`
  to build the generated addon with node-gyp, or copy a prebuilt
  binary in as `index.node`.
- **`dlopen: ... image not found`** — the addon links against the
  Rust cdylib at runtime; set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the cdylib next to `index.node`.
- **`BigInt` errors with `handle`** — handles are 64-bit; pass them as
  `bigint`, not `number`.
- **TypeScript complains about missing types** — point `tsconfig`'s
  `paths` at `generated/node/types.d.ts` or include the generated
  package in `compilerOptions.types`.
