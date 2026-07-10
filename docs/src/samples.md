# Samples

This repo includes sample projects under `samples/` that showcase end-to-end
usage of WeaveFFI. Every producer is written as safe Rust and annotated with the
`#[weaveffi::module]` family of attributes, so the macro generates its C ABI
(see [The Rust Producer Macro](guides/producer-macro.md)). The simpler producers
(`calculator`, `contacts`, and `inventory`) generate bindings straight from
their annotated source. The advanced samples (`async-demo`, `events`, `kvstore`,
`shapes`) are macro-annotated too, and they keep a committed YAML IDL as the
generation source of truth because their surfaces carry metadata the extractor
does not yet recover from source, such as package and per-generator
configuration and standalone `since` tags.

## Kvstore (kitchen-sink reference)

Path: `samples/kvstore`

A production-quality, in-memory key/value store that exercises **every IDL
feature WeaveFFI supports** in a single sample. Use this as the canonical
reference when learning the IDL surface or when you need to copy/paste a
real-world pattern for a new generator.

**What it demonstrates:**

- A first-class interface (`Store`) with a throwing constructor (`open`),
  instance methods, a static (`default_capacity`), and implicit destroy
- A struct (`Entry`) with every primitive: `i64`, `string`, `bytes`, optional
  field (`expires_at: i64?`), list field (`tags: [string]`), and map field
  (`metadata: {string:string}`), plus per-field doc strings and `builder: true`
- A documented enum (`EntryKind` with `Volatile`, `Persistent`, `Encrypted`)
- A documented error domain (`KvError` with `KeyNotFound`, `Expired`,
  `StoreFull`, `IoError`) and opt-in `throws: true` on the fallible methods
- A module-level callback (`OnEvict`) and listener (`eviction_listener`)
- A streaming iterator return (`list_keys -> iter<string>`) with prefix filter
- A cancellable async method (`compact`, `async: true, cancellable: true`)
  that respects a `weaveffi_cancel_token` while reclaiming bytes on a worker
  thread
- A deprecated method (`legacy_put`)
- A nested sub-module (`kv.stats`) with its own struct (`Stats`) and a function
  that takes a cross-module `Store` parameter
- Inline `generators:` overrides for `wasm.allow_unsupported`,
  `swift.module_name`, `cpp.namespace`, `dotnet.namespace`,
  `dart.package_name`, `go.module_path`, and `ruby.module_name`

**Build, generate bindings, and run the C ABI tests:**

```bash
cargo build -p kvstore
cargo test -p kvstore
weaveffi generate samples/kvstore/kvstore.yml -o generated
```

The `conformance/` harness ships a kvstore consumer for every language that
opens a `Store`, round-trips entries, drives the async `compact`, and asserts
the typed `KvError` surface; see `conformance/run.sh`.

## Shapes (rich enums + numerics)

Path: `samples/shapes`

The reference sample for **rich (algebraic) enums** (sum types whose variants
carry associated data) and the **expanded numeric primitives**. Use it when
learning how a tagged union crosses the C ABI as an opaque object and how each
backend wraps it.

**What it demonstrates:**

- A rich enum (`Shape`) with a data-less variant (`Empty`) and three payload
  variants (`Circle { radius: f64 }`, `Rectangle { width: f32, height: f32 }`,
  and `Labeled { label: string, count: u8 }`) lowered to an opaque object with
  per-variant constructors, a `tag` reader, per-variant field getters, and a
  destructor
- A plain C-style enum (`Channel`) alongside the rich enum, showing both enum
  flavors in one module
- The new numeric primitives (`f32`, `u8`, `u64`) as variant fields, parameters,
  and return types
- Functions that take and return a rich enum (`describe`, `scale`) and a
  list-of-bytes reduction (`sum_bytes(values: [u8]) -> u64`)

**Build, generate bindings, and run the C ABI tests:**

```bash
cargo build -p shapes
cargo test -p shapes
weaveffi generate samples/shapes/shapes.yml -o generated
```

The `conformance/` harness ships a `shapes` consumer for every language that
constructs each variant, reads the tag and fields back, and round-trips through
`describe`/`scale`; see `conformance/run.sh`.

## Calculator

Path: `samples/calculator`

The simplest sample: a single `#[weaveffi::module]` with four functions that
exercise primitive types (`i32`) and string passing. Good starting point for
understanding the basic C ABI contract and the macro workflow.

**What it demonstrates:**

- Scalar parameters and return values (`i32`)
- String parameters and return values (C string ownership)
- The smallest possible typed error surface: a `#[weaveffi::error]` enum
  (`CalcError`) and one throwing function (`div` returns
  `Result<i32, CalcError>`)
- A producer written entirely as safe Rust (no hand-written FFI glue)

**Build and generate bindings (from the annotated source):**

```bash
cargo build -p calculator
weaveffi generate samples/calculator/src/lib.rs -o generated
```

This produces target-specific output under `generated/` (C headers, Swift
wrapper, Android skeleton, Node addon sources, Wasm loader). The
[Calculator tutorial](tutorials/calculator.md) walks through running C,
Node, and Swift consumers against it.

## Contacts

Path: `samples/contacts`

A CRUD-style sample with a single module, written as safe Rust and annotated
with `#[weaveffi::module]`. It exercises richer type-system features than the
calculator while writing no `unsafe` glue.

**What it demonstrates:**

- A `#[weaveffi::enumeration]` (`ContactType` with `Personal`, `Work`, `Other`)
- A `#[weaveffi::record]` (`Contact`) with generated create/destroy/getters
- Optional fields (`Option<String>` for the email)
- A `#[weaveffi::interface]` (`ContactBook`) with a `new` constructor,
  instance methods, and implicit destroy
- List return types (`Vec<Contact>` from `ContactBook::list`)
- A `#[weaveffi::error]` domain (`ContactsError`) surfaced by the throwing
  methods via `Result<Contact, ContactsError>`

**Build and generate bindings (from the annotated source):**

```bash
cargo build -p contacts
weaveffi generate samples/contacts/src/lib.rs -o generated
```

## Inventory

Path: `samples/inventory`

A richer, multi-module sample with `products` and `orders` modules, written as
safe Rust with two `#[weaveffi::module]` blocks. It exercises cross-module
references and record lists across the macro.

**What it demonstrates:**

- Two annotated modules in one crate, each with its own error domain
  (`ProductsError`, `OrdersError`)
- A `#[weaveffi::interface]` (`Catalog`) owning its product list, alongside
  free functions in the `orders` module
- A `#[weaveffi::enumeration]` (`Category`) and `#[weaveffi::record]`s
  (`Product`, `Order`, `OrderItem`)
- Optional and list fields (`Option<String>`, `Vec<String>` tags)
- A record-list return (`Catalog::search -> Vec<Product>`) and a record-list
  parameter (`create_order(items: Vec<OrderItem>)`)
- A cross-module record parameter (`orders::add_product_to_order` takes a
  `products::Product`)

**Build and generate bindings (from the annotated source):**

```bash
cargo build -p inventory
weaveffi generate samples/inventory/src/lib.rs -o generated
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
- Iterator lifecycle (`get_messages` returns a `GetMessagesIterator`, advanced
  with `_next`, freed with `_destroy`)

**Build and run tests:**

```bash
cargo build -p events
cargo test -p events
```

## Node Addon

Path: `samples/node-addon`

An N-API addon crate that loads the calculator's C ABI shared library at runtime
via `libloading` and exposes the functions as JavaScript-friendly `#[napi]`
exports. It shows the hand-rolled alternative to the generated
`weaveffi_addon.c`, which the Node generator now emits for you.

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

The `conformance/` directory is the end-to-end regression oracle for
the code generators. Every consumer under `conformance/<language>/`
binds through the *generated* wrappers (not the raw C ABI) and asserts
concrete results against the contacts, events, kvstore, and shapes
samples. The `conformance/run.sh` harness builds each producer cdylib,
runs `weaveffi generate` for it, then compiles and runs every
per-(language, sample) consumer:

```bash
bash conformance/run.sh
```

It prints `[OK] {target}` for each consumer that succeeds and reports
a pass/fail summary at the end. Use `ONLY=c-contacts,cpp-contacts` to
run a subset, or `SKIP=go-contacts` to omit individual targets.
Missing toolchains cause the affected target to fail; skip those
explicitly. See the comment block at the top of `conformance/run.sh`
for the per-target prerequisites.
