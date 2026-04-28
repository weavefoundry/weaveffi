# Samples

This repo includes sample projects under `samples/` that showcase end-to-end
usage of the C ABI layer. Each sample contains a YAML IDL and a Rust crate
that implements the corresponding `weaveffi_*` C ABI functions.

## Kvstore (kitchen-sink reference)

Path: `samples/kvstore`

A production-quality, in-memory key/value store that exercises **every IDL
feature WeaveFFI supports** in a single sample. Use this as the canonical
reference when learning the IDL surface or when you need to copy/paste a
real-world pattern for a new generator.

**What it demonstrates:**

- Typed handles (`handle<Store>`) for opaque resource lifecycle
- A struct (`Entry`) with every primitive — `i64`, `string`, `bytes`, optional
  field (`expires_at: i64?`), list field (`tags: [string]`), and map field
  (`metadata: {string:string}`) — plus per-field doc strings and `builder: true`
- A documented enum (`EntryKind` with `Volatile`, `Persistent`, `Encrypted`)
- A documented error domain (`KvError` with `KEY_NOT_FOUND`, `EXPIRED`,
  `STORE_FULL`, `IO_ERROR`)
- A module-level callback (`OnEvict`) and listener (`eviction_listener`)
- A streaming iterator return (`list_keys -> iter<string>`) with prefix filter
- A cancellable async function (`compact_async`, `async: true, cancellable: true`)
  that respects a `weaveffi_cancel_token` while reclaiming bytes on a worker
  thread
- A deprecated function (`legacy_put`) and `since: "0.3.0"` on every other
  function
- A nested sub-module (`kv.stats`) with its own struct (`Stats`) and a function
  that takes a cross-module `handle<Store>`
- Inline `generators:` overrides for `swift.module_name`, `cpp.namespace`,
  `dotnet.namespace`, `dart.package_name`, `go.module_path`, and
  `ruby.module_name`

**Build, generate bindings, and run the C ABI tests:**

```bash
cargo build -p kvstore
cargo test -p kvstore
weaveffi generate samples/kvstore/kvstore.yml -o generated
```

Every consumer language under `examples/` ships with a kvstore smoke test
(`open -> put -> get -> delete -> close`) that runs against the generated
bindings and the produced `libkvstore` cdylib; see `examples/run_all.sh`.

## Calculator

Path: `samples/calculator`

The simplest sample — a single module with four functions that exercise
primitive types (`i32`) and string passing. Good starting point for
understanding the basic C ABI contract.

**What it demonstrates:**

- Scalar parameters and return values (`i32`)
- String parameters and return values (C string ownership)
- Error propagation via `weaveffi_error` (e.g. division by zero)
- The `weaveffi_free_string` / `weaveffi_error_clear` lifecycle helpers

**Build and generate bindings:**

```bash
cargo build -p calculator
weaveffi generate samples/calculator/calculator.yml -o generated
```

This produces target-specific output under `generated/` (C headers, Swift
wrapper, Android skeleton, Node addon loader, WASM stub). Runnable examples
that consume the generated output are in `examples/`.

## Contacts

Path: `samples/contacts`

A CRUD-style sample with a single module that exercises richer type-system
features than the calculator.

**What it demonstrates:**

- Enum definitions (`ContactType` with `Personal`, `Work`, `Other`)
- Struct definitions (`Contact` with typed fields)
- Optional fields (`string?` for the email)
- List return types (`[Contact]`)
- Handle-based resource management (`create_contact` returns a handle)
- Struct getter and setter functions
- Enum conversion functions (`from_i32` / `to_i32`)
- Struct destroy and list-free lifecycle functions

**Build and generate bindings:**

```bash
cargo build -p contacts
weaveffi generate samples/contacts/contacts.yml -o generated
```

## Inventory

Path: `samples/inventory`

A richer, multi-module sample with `products` and `orders` modules that
exercises cross-module struct references and nested list types.

**What it demonstrates:**

- Multiple modules in a single IDL
- Enums (`Category` with `Electronics`, `Clothing`, `Food`, `Books`)
- Structs with optional fields, list fields (`[string]` tags), and float types
- List-returning search functions (`search_products` filtered by category)
- Cross-module struct passing (`add_product_to_order` takes a `Product`)
- Nested struct lists (`Order.items` is `[OrderItem]`)
- Full CRUD operations across both modules

**Build and generate bindings:**

```bash
cargo build -p inventory
weaveffi generate samples/inventory/inventory.yml -o generated
```

## Async Demo

Path: `samples/async-demo`

Demonstrates the async function pattern using callback-based invocation. Async
functions in the YAML definition get an `_async` suffix at the C ABI layer and
accept a callback + context pointer instead of returning directly.

**What it demonstrates:**

- Async function declarations (`async: true` in the YAML)
- Callback-based C ABI pattern (`weaveffi_tasks_run_task_async`)
- Callback type definitions (`weaveffi_tasks_run_task_callback`)
- Batch async operations (`run_batch` processes a list of names sequentially)
- Synchronous fallback functions (`cancel_task` is non-async in the same module)
- Struct return types through callbacks (`TaskResult` delivered via callback)

**Build and run tests:**

```bash
cargo build -p async-demo
cargo test -p async-demo
```

> **Note:** Async functions are fully supported by the validator. This
> sample focuses on the C ABI callback pattern; see the
> [Async Functions guide](guides/async.md) for the per-target async/await
> story.

## Events

Path: `samples/events`

Demonstrates callbacks, event listeners, and iterator-based return types.

**What it demonstrates:**

- Callback type definitions (`OnMessage` callback)
- Listener registration and unregistration (`message_listener`)
- Event-driven patterns (sending a message triggers the registered callback)
- Iterator return types (`iter<string>` in the YAML)
- Iterator lifecycle (`get_messages` returns a `MessageIterator`, advanced with
  `_next`, freed with `_destroy`)

**Build and run tests:**

```bash
cargo build -p events
cargo test -p events
```

## Node Addon

Path: `samples/node-addon`

An N-API addon crate that loads the calculator's C ABI shared library at runtime
via `libloading` and exposes the functions as JavaScript-friendly `#[napi]`
exports. Used by the Node.js example in `examples/`.

**What it demonstrates:**

- Dynamic loading of a `weaveffi_*` shared library from JavaScript
- Mapping C ABI error structs to N-API errors
- String ownership across the FFI boundary (CString in, CStr out, free)

**Build (requires the calculator library first):**

```bash
cargo build -p calculator
cargo build -p weaveffi-node-addon
```

## End-to-end testing

Every consumer language under `examples/` ships with an executable
test that loads the calculator and contacts cdylibs at runtime and
asserts a representative slice of the C ABI (basic add, contact
create/list/cleanup). The `examples/run_all.sh` orchestrator builds
and runs each one in turn:

```bash
cargo build -p calculator -p contacts

WEAVEFFI_LIB=target/debug/libcalculator.dylib \
  bash examples/run_all.sh
```

It prints `[OK] {target}` for each example that succeeds and exits
non-zero on the first failure. Use `ONLY=python,ruby` to run a
subset, or `SKIP=android,go` to omit individual targets. CI runs the
full matrix on Linux, most targets on macOS, and the Python path on
Windows. See the comment block at the top of `examples/run_all.sh`
for the full list of env vars and per-target prerequisites.
