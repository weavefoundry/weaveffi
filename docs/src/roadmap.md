# Roadmap

WeaveFFI is in active `0.x` development. Schema 0.11 and ABI revision 4 made
names global, replaced the per-module checksums with per-declaration contract
tables, gave callback methods rich returns, typed `throws`, optional callback
parameters, and a size-checked vtable header, made `{prefix}_alloc` the one
allocator contract on every target, moved the codec conformance lanes onto
shared test vectors, and taught the CLI to read a Rust producer's API from
its built library. This page lists what comes next. Items are **planned**
(the design is settled) or **exploring** (wanted, with open design
questions). Nothing carries a date; the
[changelog](https://github.com/weavefoundry/weaveffi/blob/main/CHANGELOG.md)
records what shipped.

## Callback interfaces

### Async callback methods (planned)

A callback method that returns a future on the consumer side needs a
completion flowing the other way and a cancellation story when the producer
drops the future. The vtable shape is simple (a completion function and
context per async method); the hard part is one producer working the same way
whether the consumer runtime is an event loop, a thread pool, or the
single-threaded Wasm host.

### Off-thread callbacks in Dart (exploring)

Dart can't run a value-returning callback method synchronously on a thread
that isn't a Dart isolate thread, so a producer that calls one from its own
worker thread aborts the process today (void methods are forwarded to the
isolate safely). Pure `dart:ffi` can't route that return, so the fix is
either a small native shim that hops to the isolate and waits, or a
per-vtable thread-affinity hint that lets the producer refuse the call with
`-4` instead. See the [Dart page](generators/dart.md#known-limitations).

### Synchronous calls that wait on Node.js callbacks (exploring)

A synchronous Node.js call that blocks the JS thread while a producer thread
waits on a callback the JS thread must run would deadlock, so the addon
gives such a callback about a second to reach the JS thread and then fails
it with `-4`. A long synchronous call that legitimately overlaps callbacks
hits that guard. Making the call itself pump callbacks while it waits would
remove the heuristic. See the [Node.js page](generators/node.md#threading).

## Definitions

### Multi-file IDL (planned)

An IDL API is one document. Large APIs want to split by module, and a
monorepo wants to reference another package's types. The plan is an
`imports:` list resolved at parse time, with bare type names still unique
across the merged API, and `diff`, `validate`, and contract tables covering
every imported file.

### Generic and trait-object interfaces (exploring)

WeaveFFI has a fixed set of generic shapes (`T?`, `[T]`, `{K:V}`, `iter<T>`).
Under discussion are trait-object interfaces (one declared method set with
several producer implementations behind `Arc<dyn Trait>`) and, less likely,
parameterized interfaces monomorphized per instantiation.

### Duration and timestamp primitives (exploring)

Producers pass time as `i64` with a documented unit. `duration` and
`timestamp` primitives mapped to each language's types would remove the
ambiguity; the open question is the representation.

### Custom types (exploring)

A producer type that crosses as a builtin (a `Uuid` as `string`, a `Url` as
`string`, a fixed-point amount as `i64`) has to be converted by hand on both
sides today. A declared custom type with per-language conversion hooks would
let each binding expose the language's own type.

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

### Zero-copy blittable records (exploring)

Every record crosses as a serialized value buffer. A record of fixed-size
scalars could instead cross as a `#[repr(C)]` struct passed by pointer, which
matters for hot paths that move many small records. The open questions are
how a record opts in, and how the contract hash and every target's layout
checks keep the two sides agreeing on the layout.

### Poll-based async (exploring)

Async functions complete through a callback the producer fires on its own
thread, and each binding hops back to its scheduler. A poll-based protocol,
where the consumer's runtime drives the future and the producer only wakes
it, would fit event-loop runtimes better and avoid that hop. It would be an
option next to the completion-callback ABI, not a replacement.

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
PowerShell equivalents), installing each toolchain on Windows runners, and
prebuilding the Node.js addon and JNI shim for `windows-x64`, which
`weaveffi build` can't do yet.

## Toward 1.0

1.0 means the surfaces in [What 1.0 will cover](stability.md#what-10-will-cover)
stop changing without a major release. It needs:

- ABI revision 4 stable for several releases, so 1.0 doesn't need
  revision 5;
- every target passing the full conformance matrix on Linux, macOS, and
  Windows;
- multi-file IDL and the deprecation policy in place;
- a schema migration tool and more than one accepted schema version;
- the known limitations on each generator page resolved or documented as
  permanent;
- a review of every published crate's public API.

## Contributing

Open an issue or discussion on
[GitHub](https://github.com/weavefoundry/weaveffi/issues) if an item here
matters to you or something you need is missing.
