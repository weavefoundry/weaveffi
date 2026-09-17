# Samples

This repo includes eight sample projects under `samples/` that showcase
end-to-end usage of WeaveFFI. Every producer is written as safe Rust and
annotated with the `#[weaveffi::module]` family of attributes, so the macro
generates its C ABI (see [The Rust Producer Macro](guides/producer-macro.md)),
and every sample generates its bindings straight from that annotated
`src/lib.rs`: there is no parallel IDL file to keep in sync. Package identity
and per-target options live in each sample's `weaveffi.toml`, which the CLI
discovers automatically next to the crate.

Each sample builds as a `cdylib`, ships Rust tests that drive its own exported
C symbols the way a foreign consumer would, and is regenerated in CI with
`weaveffi diff --check` so the committed source and the generators can't drift.

| Sample        | Path                  | Showcases                                                                      |
|---------------|-----------------------|--------------------------------------------------------------------------------|
| `calculator`  | `samples/calculator`  | The smallest module: scalars, strings, one error domain                        |
| `contacts`    | `samples/contacts`    | A record, a C-style enum, an interface with CRUD methods, list returns         |
| `inventory`   | `samples/inventory`   | Two modules, cross-module records, an interface owning a collection            |
| `shapes`      | `samples/shapes`      | Rich (algebraic) enums and the full numeric primitive set                      |
| `async-demo`  | `samples/async-demo`  | Async functions returning records and lists                                    |
| `events`      | `samples/events`      | A callback interface, a shared reference-counted object, an iterator           |
| `kvstore`     | `samples/kvstore`     | Every IDL feature, including an object graph and an eviction listener          |
| `codec`       | `samples/codec`       | A round-trip oracle for every value-buffer wire shape, object tokens included  |

## Kvstore (kitchen-sink reference)

Path: `samples/kvstore`

A production-quality, in-memory key/value store that exercises **every IDL
feature WeaveFFI supports** in a single sample. Use this as the canonical
reference when learning the IDL surface or when you need to copy a real-world
pattern for a new generator.

**What it demonstrates:**

- A reference-counted interface (`Store`) with a throwing constructor
  (`open`), instance methods, statics (`default_capacity`, `open_many`,
  `total_count`), and the implicit `_clone`/`_destroy` pair
- An **object graph**: `share` returns a second reference to the same store
  (`self: Arc<Self>` in, `Arc<Store>` out), `fork` returns a new store,
  `larger(other: Store?) -> Store?` takes and returns an optional object,
  `describe -> StoreInfo` returns a record whose `store` field carries the
  object itself (and an optional `mirror`), `open_many -> [Store]` returns a
  list of objects, and `total_count([Store], StoreInfo?)` takes objects inside
  a list and inside an optional record
- A **callback interface** (`EvictionListener`) attached with
  `set_eviction_listener` and detached with `clear_eviction_listener`; the
  store retains the consumer's implementation, notifies it outside every
  lock, and drops it when `on_evict` returns `false`
- A record (`Entry`) with every primitive: `i64`, `string`, `bytes`, an
  optional field (`expires_at: i64?`), a list field (`tags: [string]`), and a
  map field (`metadata: {string:string}`), plus per-field doc strings
- Documented C-style enums (`EntryKind`, `EvictionReason`)
- A documented error domain (`KvError` with `KeyNotFound`, `Expired`,
  `StoreFull`, `IoError`) and opt-in `throws: true` on the fallible methods
- A streaming iterator return (`list_keys -> iter<string>`) with a prefix
  filter
- A cancellable async method (`compact`, `#[weaveffi::cancellable]`) that
  checks its `CancelToken` before reclaiming expired entries
- A deprecated method (`legacy_put`, via `#[deprecated(note = ...)]`)
- A nested sub-module (`kv.stats`) with its own record (`Stats`) and a
  function that takes the parent module's `Store` by reference
- A `weaveffi.toml` with `[generators.<target>]` overrides for
  `swift.module_name`, `cpp.namespace`, `dotnet.namespace`,
  `dart.package_name`, `go.module_path`, and `ruby.module_name`

**Build, generate bindings, and run the C ABI tests:**

```bash
cargo build -p kvstore
cargo test -p kvstore
weaveffi generate samples/kvstore/src/lib.rs -o generated
```

The `conformance/` harness ships a kvstore consumer for every language that
opens a `Store`, round-trips entries, shares and forks stores, attaches an
eviction listener, drives the async `compact`, and asserts the typed `KvError`
surface; see `conformance/run.sh`.

## Events (callback interface + shared object)

Path: `samples/events`

A publish/subscribe bus and the reference sample for **callback interfaces**
and **object sharing**. The consumer implements `Subscriber`; the bus retains
any number of subscribers, asks each how to `route` a message, delivers
accepted messages, and hands the bus itself back through a callback.

**What it demonstrates:**

- A callback interface (`Subscriber`) with three method shapes: `route(topic:
  string) -> Delivery` returns a C-style enum, `on_message(message: Message)
  -> i64` takes a record and returns a scalar, and `on_attached(bus:
  EventBus)` receives an object the consumer adopts
- A reference-counted interface (`EventBus`) whose constructor returns
  `Arc<Self>` and whose `subscribe` method takes `self: Arc<Self>` so it can
  hand a strong reference to the subscriber
- Producer-side discipline for calling into consumer code: the bus snapshots
  its subscriber list and never holds a lock across a callback, because a
  failing subscriber aborts the publishing call with `FOREIGN_ERROR_CODE`
- Consumer `free` semantics: `clear_subscribers` (or destroying the bus)
  releases the producer's references and each consumer implementation's
  `free` entry runs exactly once
- An async method (`publish_later`), an iterator return (`messages ->
  iter<string>`), an optional record return (`last_message -> Message?`), and
  a free function taking a callback interface (`route_once`)

**Build and run tests:**

```bash
cargo build -p events
cargo test -p events
weaveffi generate samples/events/src/lib.rs -o generated
```

The Rust tests build a `Subscriber` vtable by hand, exactly as a generated
binding does, and assert reference counts, `free` calls, and the foreign-error
path. The `conformance/` harness runs an `events` consumer in all eleven
languages.

## Codec (value-buffer round-trip oracle)

Path: `samples/codec`

Every generated binding ships its own encoder and decoder for the
[value-buffer protocol](reference/value-buffers.md). This sample gives the
conformance harness one producer that exercises **every wire shape in both
directions**, so a codec bug in any language shows up as a concrete mismatch
rather than a subtle corruption.

**What it demonstrates:**

- `Scalars`: a record with every fixed-width scalar (`i8` through `u64`,
  `f32`, `f64`, `bool`) and a C-style enum, using edge values such as
  `u64::MAX`, `i64::MIN`, and a non-integer `f64`
- `Composite`: a record with strings (including non-ASCII), bytes, present
  and absent optionals, lists, lists of lists, an empty list, string-keyed and
  integer-keyed maps, a nested record, a rich enum, a list of rich enums, an
  optional rich enum, an optional list, a list of optionals, and a list of
  enums
- `Shape`: a rich enum with unit, scalar, mixed, string, and nested-record
  variants
- `Holder`: **objects inside buffers** (a required `Token`, an optional one,
  and a list of them), with `make_holder`, `sum_holder`, `primary_of`, and
  `same_primary` proving that each token carries one strong reference, that a
  buffer is decoded exactly once, and that identity survives the round trip
- Three function families per shape: `sample_*` (producer encodes, consumer
  decodes), `verify_*` (consumer encodes, producer decodes and fails with
  `CodecError::Mismatch` on any difference), and `roundtrip_*` (echo), plus
  `describe_*` helpers that render what the producer actually saw

**Build and run tests:**

```bash
cargo build -p codec
cargo test -p codec
weaveffi generate samples/codec/src/lib.rs -o generated
```

The `conformance/` harness runs the `codec` consumer in all eleven languages;
it is the lane to watch when touching any generator's buffer code.

## Shapes (rich enums + numerics)

Path: `samples/shapes`

The reference sample for **rich (algebraic) enums** (sum types whose variants
carry associated data) and the **expanded numeric primitives**. Use it when
learning how a tagged union crosses the C ABI serialized in a value buffer and
how each backend surfaces it as an idiomatic sum type.

**What it demonstrates:**

- A rich enum (`Shape`) with a data-less variant (`Empty`) and three payload
  variants (`Circle { radius: f64 }`, `Rectangle { width: f32, height: f32 }`,
  and `Labeled { label: string, count: u8 }`), serialized on the wire as an
  `i32` tag followed by the active variant's fields
- A plain C-style enum (`Channel`) alongside the rich enum, showing both enum
  flavors in one module
- The numeric primitives `f32`, `u8`, and `u64` as variant fields, parameters,
  and return types
- Functions that take and return a rich enum (`describe`, `scale`) and a
  list-of-bytes reduction (`sum_bytes(values: [u8]) -> u64`)

**Build, generate bindings, and run the C ABI tests:**

```bash
cargo build -p shapes
cargo test -p shapes
weaveffi generate samples/shapes/src/lib.rs -o generated
```

The `conformance/` harness ships a `shapes` consumer for every language that
constructs each variant, reads the tag and fields back, and round-trips through
`describe`/`scale`.

## Calculator

Path: `samples/calculator`

The simplest sample: a single `#[weaveffi::module]` with four functions that
exercise primitive types (`i32`) and string passing. Good starting point for
understanding the basic C ABI contract and the macro workflow.

**What it demonstrates:**

- Scalar parameters and return values (`i32`)
- String parameters and return values (borrowed in, producer-owned out, freed
  with `weaveffi_free_string`)
- The smallest possible typed error surface: a `#[weaveffi::error]` enum
  (`CalcError`) and one throwing function (`div` returns
  `Result<i32, CalcError>`)
- A producer written entirely as safe Rust (no hand-written FFI glue)

**Build and generate bindings (from the annotated source):**

```bash
cargo build -p calculator
weaveffi generate samples/calculator/src/lib.rs -o generated
```

This produces target-specific output under `generated/` for all eleven
languages. The [Calculator tutorial](tutorials/calculator.md) walks through
running C, Node, and Swift consumers against it.

## Contacts

Path: `samples/contacts`

A CRUD-style sample with a single module. It exercises richer type-system
features than the calculator while writing no `unsafe` glue.

**What it demonstrates:**

- A `#[weaveffi::enumeration]` (`ContactType` with `Personal`, `Work`, `Other`)
- A `#[weaveffi::record]` (`Contact`) with a generated `BufferValue`
  encode/decode impl
- Optional fields (`Option<String>` for the email)
- A `#[weaveffi::interface]` (`ContactBook`) with a `new` constructor,
  `&self` methods guarding a `Mutex`, and the implicit `_clone`/`_destroy`
  pair
- List return types (`Vec<Contact>` from `ContactBook::list`)
- A `#[weaveffi::error]` domain (`ContactsError`) surfaced by the throwing
  methods via `Result<Contact, ContactsError>`

**Build and generate bindings (from the annotated source):**

```bash
cargo build -p contacts
weaveffi generate samples/contacts/src/lib.rs -o generated
```

The `conformance/` harness runs a `contacts` consumer in all eleven languages.

## Inventory

Path: `samples/inventory`

A richer, multi-module sample with `products` and `orders` modules, written as
safe Rust with two `#[weaveffi::module]` blocks. It exercises cross-module
references and record lists.

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

The `conformance/` harness runs `inventory` consumers in C and Python.

## Async Demo

Path: `samples/async-demo`

Demonstrates the async function pattern. An `async fn` export lowers to an
`_async` launcher at the C ABI that accepts a completion callback plus a
context pointer instead of returning directly, and each target wraps it in its
native awaitable.

**What it demonstrates:**

- Async exports (`pub async fn` under `#[weaveffi::export]`) returning a
  record (`run_task -> TaskResult`), a list of records (`run_batch`), and a
  scalar (`run_n_tasks`)
- A throwing async function (`run_task` returns
  `Result<TaskResult, TaskError>`), delivered as a heap-boxed error the
  consumer frees with `weaveffi_error_free`
- Synchronous functions in the same module (`cancel_task`,
  `active_callbacks`)
- The default spawner (one thread per future) in action; see the
  [Async Functions guide](guides/async.md) for plugging in Tokio with
  `weaveffi::set_spawner`

**Build and run tests:**

```bash
cargo build -p async-demo
cargo test -p async-demo
```

The `conformance/` harness runs an `async-demo` consumer in all eleven
languages, awaiting the results through each target's native idiom.

## End-to-end testing

The `conformance/` directory is the end-to-end regression oracle for the code
generators. Every consumer under `conformance/<language>/` binds through the
*generated* wrappers (not the raw C ABI) and asserts concrete results against
the samples: `contacts`, `events`, `kvstore`, `shapes`, `codec`, and
`async-demo` in all eleven languages, plus `inventory` in C and Python. The
`conformance/run.sh` harness builds each producer cdylib, runs
`weaveffi generate` for it, then compiles and runs every per-(language,
sample) consumer:

```bash
bash conformance/run.sh
```

It prints `[OK] {target}` for each consumer that succeeds and reports a
pass/fail summary at the end. Use `ONLY=c-contacts,cpp-contacts` to run a
subset, or `SKIP=go-contacts` to omit individual targets. Missing toolchains
cause the affected target to fail; skip those explicitly. See the comment
block at the top of `conformance/run.sh` for the per-target prerequisites.
