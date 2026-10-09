# Samples

The `samples/` directory holds three Rust producers written with
`#[weaveffi::module]`: a minimal one, a wire-format oracle, and a
feature-complete one. Each is a workspace crate built as a `cdylib` with the
`leak-check` feature on, has unit tests that call it directly, and is
consumed by the [conformance harness](generators/README.md#conformance-lanes)
in all eleven languages, one consumer per sample per language.

```bash
weaveffi generate samples/kvstore -o bindings
```

Each sample's crate name is its C prefix: `kvstore` exports
`kvstore_kv_Store_open`, and its bindings load `libkvstore` (the targets
that load it at run time also accept a path in `KVSTORE_LIBRARY`). `weaveffi generate` builds the crate and reads its API
from the library; the `weaveffi.toml` beside each crate supplies package
metadata.

## calculator

The getting-started example, small enough to read in a minute: a
`calculator` module with `add` (wrapping on overflow), `divide`, which
throws the one-code `CalcError` domain on a zero divisor, and `greet`, a
string in and a string out. The `c-producer-exports` lane also implements
the calculator's generated header by hand in C (`conformance/c/producer.c`)
and checks that every required runtime and contract symbol is exported.

## codec

The oracle for the [value-buffer format](reference/value-buffers.md), built
on shared test vectors. The producer holds a fixed table of values covering
every wire shape: each primitive at its extremes (with NaN, the infinities,
`-0.0`, and subnormals for floats), empty and non-empty strings and bytes
with non-ASCII text and interior NUL bytes, optionals, lists, maps keyed by
strings, integers, and a C-style enum, nested records, C-style and rich
enums, and objects inside buffers. Each vector is a variant of the `Vector`
rich enum, so every language's codec check is one loop:

1. `vector_count()` returns how many vectors there are.
2. `vector(i)` returns vector `i` for the consumer to decode. An index past
   the end fails with `CodecError::OutOfRange { index, count }`.
3. `check_vector(i, v)` takes the decoded value back, re-encoded by the
   consumer, and returns `true` only if it's exactly vector `i` (floats keep
   their sign, and any NaN matches NaN).
4. `vector_name(i)` and `describe_vector(v)` label and render a vector, so a
   failing consumer can say what went wrong.

A round trip can't catch a decoder and an encoder that make the same mistake,
so each consumer also builds a few vectors from literals and checks those,
and spot-checks decoded fields. The `echo_*` functions return their argument
through the direct ABI families (scalars by value, strings and bytes as
`(ptr, len)` runs, the C-style enum as an `int32_t`), and each consumer pushes
every primitive vector through the matching echo. A `Token` interface with
`sum_holder`, `primary_of`, and `same_primary` checks object identity and
reference counting through buffers.

## kvstore

The feature-complete producer: an in-memory key-value store whose API uses
every feature WeaveFFI supports, arranged so a consumer can check each with
concrete assertions. Time is a logical clock per store (`now`, advanced by
`tick`), so TTLs are deterministic.

| Feature | Where |
|---|---|
| Interface with constructors, methods, and statics | `Store` (`open`, `new`; `put`, `get`, ...; `open_many`, `default_capacity`, ...) |
| Typed errors with payload fields | `KvError`: `KeyNotFound { key }`, `Expired { key, expired_at }`, `StoreFull { capacity }`, `InvalidPath`, `Rejected { key, reason }` |
| Records, optionals, lists, maps | `Entry` (bytes, an optional TTL, tags, metadata), `StoreInfo`, `Stats` (a map keyed by a C-style enum) |
| C-style and rich enums | `EntryKind`; `Change` (`Put`, `Removed`, `Cleared`) |
| Objects in every position | parameters, returns, `Store?` both ways (`larger`), lists (`open_many`, `total_count`), map values (`by_label`), record fields (`StoreInfo`), iterator elements (`partition`), an async result (`open_store`), and a callback's parameter and return (`Policy::route`) |
| Callback interfaces | `Listener` (retained by `subscribe`; void and direct returns), `Policy` (a record return, a throwing method whose typed error reaches the `put` caller, an object parameter and return), `Loader` (string, bytes, and optional-object returns; passed as an optional callback) |
| Callbacks from a producer thread | `compact` notifies listeners from the thread it runs on |
| Lazy iterators | `keys` (strings, throwing), `entries` (records), `partition` (objects) |
| Async and cancellation | `open_store` (an async free function returning an object), `compact` (cancellable; its background pause stops cooperatively, shown by `Store::active_jobs`), `get_many` (an async list), `summarize_all` |
| Nested modules | `kv.stats` uses the parent's `Store` and reports the parent's `KvError` |
| Sibling roots | `report` shares the `Entry` record and has its own `ReportError` |
| Deprecation | `Store::size` (use `count`) |

A callback's failure surfaces according to its role: a `Listener` that fails
is unsubscribed while the store operation succeeds; a `Policy` or `Loader`
failure fails the call with the callback's code, message, and payload, so a
typed `KvError` a consumer raised reaches the original caller typed. The
store never holds a lock while a callback runs, so callbacks may call back
into it.
