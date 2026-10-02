# Roadmap

WeaveFFI is in active `0.x` development. Schema 0.10 and ABI revision 3 made
every name derive from the library's identity, moved strings to `(ptr, len)`
runs, added contract checksums, and made cancellation work idiomatically on
every target. This page lists what comes next. Items are **planned** (the
design is settled) or **exploring** (wanted, with open design questions).
Nothing carries a date; the
[changelog](https://github.com/weavefoundry/weaveffi/blob/main/CHANGELOG.md)
records what shipped.

## Callback interfaces

### Rich callback returns (planned)

Callback methods return nothing or a direct value, because a consumer
allocation can't safely cross back to a producer with a different allocator.
The plan is an allocator contract for callback returns: the consumer writes
the result into producer-owned storage obtained through the runtime (as the
Wasm glue already does with `{prefix}_alloc`), or returns its own allocation
with a release function the producer calls after copying. With that,
strings, bytes, records, rich enums, optionals, lists, maps, and objects
become valid callback returns, and typed `throws` on callback methods follows.

### Async callback methods (planned)

A callback method that returns a future on the consumer side needs a
completion flowing the other way and a cancellation story when the producer
drops the future. The vtable shape is simple (a completion function and
context per async method); the hard part is one producer working the same way
whether the consumer runtime is an event loop, a thread pool, or the
single-threaded Wasm host. This follows the allocator contract above.

### Vtable versioning (exploring)

A vtable is a fixed struct, so adding a method to a callback interface is a
breaking change even when no consumer needs it. A size or version field at
the head of each vtable would let a newer producer detect an older consumer's
shorter table and fall back, which matters once 1.0 promises additive
changes are compatible.

### Callback thread affinity (exploring)

Dart can't run a value-returning callback method synchronously on a thread
other than its isolate's, so a producer that calls one from a worker thread
aborts the process today (void methods are forwarded safely). A per-vtable
thread-affinity hint, or a runtime helper that lets a consumer refuse an
off-thread call with `-4`, would turn that abort into an error.

## Definitions

### Multi-file IDL (planned)

An IDL API is one document. Large APIs want to split by module, and a
monorepo wants to reference another package's types. The plan is an
`imports:` list resolved at parse time, with bare type names still unique
across the merged API, and `diff`, `validate`, the cache, and checksums
tracking every imported file.

### Extraction from the compiled library (exploring)

The CLI reads a Rust producer's API by parsing source, so it sees only
inline `#[weaveffi::module]`s in one file and can't expand macros. The macro
already lowers each module to the IR at compile time; embedding that IR in
the built library (a custom section or an exported symbol) would let
`weaveffi generate` read the exact API from the artifact, removing the
one-file limit and any chance of the parser and the compiler disagreeing.

### Generic and trait-object interfaces (exploring)

WeaveFFI has a fixed set of generic shapes (`T?`, `[T]`, `{K:V}`, `iter<T>`).
Under discussion are trait-object interfaces (one declared method set with
several producer implementations behind `Arc<dyn Trait>`) and, less likely,
parameterized interfaces monomorphized per instantiation.

### Duration and timestamp primitives (exploring)

Producers pass time as `i64` with a documented unit. `duration` and
`timestamp` primitives mapped to each language's types would remove the
ambiguity; the open question is the representation.

## Targets and runtime

### Kotlin Multiplatform (exploring)

The Kotlin target reaches Android and the JVM through JNI. A Multiplatform
flavor using Kotlin/Native `cinterop` for iOS and desktop is the natural next
step.

### Wasm threads (exploring)

On `wasm32-unknown-unknown` futures run inline and callbacks fire only while
a call is on the stack. A spawner that schedules on the JS event loop, or
shared-memory builds with Web Workers, would lift both limits and let
Emscripten mode support async functions and callback interfaces.

### Per-language support packages (exploring)

Every package inlines its helpers (codec, error types, object base), which
keeps consumers free of dependencies but means a codec fix ships as a
regeneration. Opt-in shared support packages per ecosystem are under
consideration; inlining would stay the default.

## Testing

### Windows conformance lanes (planned)

The workspace's tests run on Windows, Linux, and macOS, but the conformance
harness runs only on Linux and macOS. Adding Windows lanes means making
`conformance/run.sh` and the per-language scripts portable (or adding
PowerShell equivalents) and installing each toolchain on Windows runners.

### Shared codec vectors (planned)

Each language's `codec` conformance consumer asserts the same round-trip
values by hand. Moving those values into one data file that every consumer
reads (or into producer-side golden values) would shrink the consumers and
keep the eleven lanes from drifting apart.

## Toward 1.0

1.0 means the surfaces in [What 1.0 will cover](stability.md#what-10-will-cover)
stop changing without a major release. It needs:

- ABI revision 3 stable for several releases, with the callback-return
  allocator contract and vtable versioning settled so 1.0 doesn't need
  revision 4;
- every target passing the full conformance matrix on Linux, macOS, and
  Windows;
- multi-file IDL and the deprecation policy in place;
- a schema migration tool and more than one accepted schema version;
- a review of every published crate's public API.

## Contributing

Open an issue or discussion on
[GitHub](https://github.com/weavefoundry/weaveffi/issues) if an item here
matters to you or something you need is missing.
