# Samples and Examples

The repository has two sets of producers. The **examples** in `examples/`
are the code this book shows: small crates whose tests call their C
functions, and IDL documents a test validates. The **samples** in
`samples/` are the producers the
[conformance harness](generators/README.md) runs in every language.

## Examples

| Example | What it is | Tested by |
|---------|------------|-----------|
| [`examples/getting-started`](https://github.com/weavefoundry/weaveffi/tree/main/examples/getting-started) | The [Getting Started](getting-started.md) producer (`add`, `div` with a typed error, `greet`) and its C consumer, `main.c` | `cargo test -p mathlib`: the C functions through an `extern "C"` block, and a check that the README's quickstart repeats the crate's source |
| [`examples/macro-tour`](https://github.com/weavefoundry/weaveffi/tree/main/examples/macro-tour) | One of each producer attribute, for [the producer macro guide](guides/producer-macro.md): functions, typed and untyped errors, records and enums, custom types, an interface with an iterator, a callback interface, and async calls with cancellation | `cargo test -p macro-tour` (and `--features admin`): every C function the attributes generate, including a callback interface implemented with a hand-built vtable and a cancelled async call |
| [`examples/idl`](https://github.com/weavefoundry/weaveffi/tree/main/examples/idl) | The IDL documents the [IDL reference](reference/idl.md) includes (`contacts.yml`, the multi-file `drawing/`, and the rejected documents), and the `greeter` project with its C producer, `greeter.c` | `cargo test -p idl-examples`: every document validates, and every rejected one fails with the code it's named after |

## Samples

Three Rust producers: a minimal one, a wire-format oracle, and a
feature-complete one. Each is a workspace crate built with the `leak-check`
feature on, has unit tests, and is consumed by the conformance harness in
all eleven languages, one consumer per sample per language.

```bash
weaveffi generate samples/kvstore -o bindings
```

Each sample's crate name is its C prefix: `kvstore` exports
`kvstore_Store_open`, and its bindings load `libkvstore` (the targets that
load it at run time also accept a path in `KVSTORE_LIBRARY`). `weaveffi
generate` builds the crate and reads its API from the library; the
`weaveffi.toml` beside each crate supplies package metadata.

### calculator

The smallest useful producer, small enough to read in a minute:

- `add` (wrapping on overflow) and `greet`, a string in and a string out;
- `divide`, which throws the `CalcError` domain on a zero divisor;
- `parse`, which throws a second domain, `ParseError`, whose `NotANumber`
  code carries the input as a field and renders its message from a
  template (`not a number: 4x`); both domains use code `1`, so a binding
  must interpret a code by the callable's domain;
- `sqrt`, a `throws: any` function returning `Result<f64, String>`, which
  fails with code `-1` and the message `cannot take the square root of -4`;
- `mean` (`[f64]` in, `f64?` out) and `running_total` (`[i32]` in and out),
  which cross as typed arrays and an optional scalar.

The C target's `c-producer-exports` lane also implements the calculator's
generated header by hand in C and checks that every runtime and contract
symbol it requires is exported.

### codec

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
through the other families, and each consumer pushes every primitive vector
through the matching echo: scalars by value (`echo_i8` ... `echo_bool`),
strings and bytes as runs (`echo_text`, `echo_blob`), the C-style enum as an
`int32_t` (`echo_color`), optional scalars as `{p}_opt_*` structs
(`echo_opt_i32`, `echo_opt_f64`, `echo_opt_bool`, and `echo_opt_color`,
including `-0.0`, NaN, and absent), typed arrays (`echo_f64s`, `echo_i32s`,
and `echo_u64s`, bit-exact at the extremes), a Rust `usize` as `u64`
(`echo_usize`), a one-scalar string (`echo_char`; `"ab"` fails with `-3`),
and a custom type crossing as a lowercase hex string (`echo_hex`; `"xyz"`
fails with `-3`). `chunks` is an iterator of typed arrays, and a `Token`
interface with `sum_holder`, `primary_of`, and `same_primary` checks object
identity and reference counting through buffers.

### kvstore

The feature-complete producer: an in-memory key-value store whose API uses
every feature WeaveFFI supports, arranged so a consumer can check each with
concrete assertions. Time is a logical clock per store (`now`, advanced by
`tick`), so TTLs are deterministic.

| Feature | Where |
|---|---|
| Interface with constructors, methods, and statics | `Store` (`open`, `new`; `put`, `get`, ...; `open_many`, `default_capacity`, ...) |
| Typed errors with payload fields and generated messages | `KvError`: `KeyNotFound { key }`, `Expired { key, expired_at }`, `StoreFull { capacity }`, `InvalidPath`, `Rejected { key, reason }`, `CallbackFailed { message }` |
| An untyped (`throws: any`) error | `import_lines` (a `Result<usize, String>`) |
| Optional scalars and typed arrays | `put`'s TTL (an `i64?` parameter), `expires_at` (an `i64?` return), `value_sizes` (a `[u64]` return), `expirations` (an iterator of `i64?`), `version_of` and `versions` (async `u32?` and `[u32]` results), and `Scorer::scores` (a `[u64]` parameter and `[f64]` return) |
| `usize` | `count` and `listener_count` (`u64` in the IDL) |
| Records, optionals, lists, maps | `Entry` (bytes, an optional TTL, tags, metadata), `StoreInfo`, `Stats` (a map keyed by a C-style enum) |
| C-style and rich enums | `EntryKind`; `Change` (`Put`, `Removed`, `Cleared`) |
| Objects in every position | parameters, returns, `Store?` both ways (`larger`), lists (`open_many`, `total_count`), map values (`by_label`), record fields (`StoreInfo`), iterator elements (`partition`), an async result (`open_store`), and a callback's parameter and return (`Policy::route`) |
| Callback interfaces | `Listener` (retained by `subscribe`), `Policy` (`ttl_for`, an optional scalar in and out; `admit`, a record return whose typed `KvError` reaches the `put` caller; `route`, an object parameter and return), `Loader` (string, bytes, and optional-object returns; passed as an optional callback), `Scorer` (typed arrays in and out, used by `rank`) |
| Callbacks from a producer thread | `compact` notifies listeners from the thread it runs on |
| Lazy iterators | `keys` (strings, throwing), `entries` (records), `partition` (objects), `expirations` (optional scalars) |
| Async and cancellation | `open_store` (an async free function returning an object), `compact` (cancellable; its background pause stops cooperatively, shown by `Store::active_jobs`), `get_many` (an async list), `summarize_all` |
| Declarations in other Rust modules | `stats` (`Stats`, `summarize`, `summarize_all`) and `report` (`render_report`, with its own `ReportError` domain) join the library's one namespace |
| Deprecation | `Store::size` (use `count`) |

A callback's failure surfaces according to its role: a `Listener` that fails
is unsubscribed while the store operation succeeds; a `Policy`, `Loader`, or
`Scorer` failure fails the call. A typed `KvError` a consumer raised (with
its fields) reaches the original caller typed, its message rendered from the
fields, and any other consumer failure becomes `KvError::CallbackFailed`
carrying the consumer's message, through the domain's `From<ForeignError>`
implementation. The store never holds a lock while a callback runs, so
callbacks may call back into it.
