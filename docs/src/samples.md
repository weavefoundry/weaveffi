# Samples

The `samples/` directory holds six Rust producers written with
`#[weaveffi::module]`. Each is a workspace crate built as a `cdylib` with the
`leak-check` feature on, has unit tests that call its exported symbols
directly, and is consumed by the [conformance harness](generators/README.md#conformance-lanes)
in all eleven languages.

```bash
cargo build -p kvstore
weaveffi generate samples/kvstore/src/lib.rs -o generated
```

Each sample's crate name is its C prefix: `kvstore` exports
`kvstore_kv_Store_open`, and its bindings load `libkvstore` (or
`KVSTORE_LIBRARY`). The `weaveffi.toml` beside each crate supplies package
metadata.

## calculator

The smallest producer: a `calculator` module with `add`, `mul`, `echo`
(a string round trip), and `div`, which throws the one-code `CalcError`
domain. Start here to see the shape of a producer and its tests. The
`c-producer-exports` lane also implements the calculator's generated header
by hand in C (`conformance/c/producer.c`) and checks that every required
runtime and checksum symbol is exported.

## contacts

An address book built around a `ContactBook` interface whose methods guard
their state with a `Mutex` and throw `ContactsError` codes. It shows records
(`Contact`, with an optional email), a C-style enum (`ContactType`), lists of
records, and both ways modules relate:

- `contacts::groups` is nested under the `contacts` root, so it uses the
  parent's `ContactBook` interface and `ContactType` enum, returns new
  `ContactBook` objects, and inherits the parent's error domain.
- `directory` is a sibling root with its own `DirectoryError`. Across roots
  only value types can be shared, so it takes the `Contact` record.

## events

A publish/subscribe bus: an `EventBus` interface, a consumer-implemented
`Subscriber` callback interface, a `Message` record, and a `Delivery` enum.
It shows both callback styles: `route` returns
`Result<Delivery, ForeignError>` and receives consumer failures as values,
while `on_message` and `on_attached` (which hands the bus itself to the
consumer) return plain values. It also has an iterator over published
messages (`messages`), an async method (`publish_later`), an optional record
return (`last_message`), and a free function taking a callback
(`route_once`). The bus snapshots its subscribers before calling out and
never holds a lock across a callback.

## kvstore

The kitchen-sink reference: an in-memory key-value store that uses every
feature WeaveFFI supports. The `kv` module has a `Store` interface with a
throwing constructor, methods, and statics; a `KvError` domain; records with
optional, list, map, and bytes fields; `StoreInfo`, a record carrying `Store`
objects; methods that pass objects in every position (`share`, `fork`,
`larger` with `Store?` in and out, `describe`, `open_many` returning
`[Store]`, `total_count`); an `EvictionListener` callback interface; a
throwing iterator (`list_keys`); a cancellable async method (`compact`); a
deprecated method (`legacy_put`); and a nested `kv.stats` module that takes
the parent's `Store`.

## async-demo

Async functions in the `tasks` module: `run_task` (an async throwing function
returning a record), `run_batch` (an async list), `run_n_tasks` (for stress
tests of concurrent launches), and `wait`, a cancellable timer. Cancelling
`wait` completes the call with `-5` immediately and drops the pending timer;
`active_callbacks` returns to zero once every task body has finished or been
cancelled, which the lanes assert. The futures need no async runtime, so the
default spawner drives them.

## codec

A round-trip oracle for the [value-buffer format](reference/value-buffers.md).
`sample_*` functions return canonical values the consumer checks field by
field (producer encodes, consumer decodes); `verify_*` functions take the
same values back and fail with `CodecError::Mismatch` unless they decode
exactly (consumer encodes, producer decodes); `roundtrip_*` functions echo
scalars, 64-bit edge values, strings, bytes, optionals, maps, rich enums, and
records; and `describe_*` functions render what the producer saw. A `Token`
interface and a `Holder` record cover object tokens inside buffers.
