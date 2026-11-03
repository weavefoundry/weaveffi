# Roadmap

WeaveFFI is in active `0.x` development. This page lists what comes next.
Items are **planned** (the design is settled) or **exploring** (wanted, with
open design questions). Nothing carries a date; the
[changelog](https://github.com/weavefoundry/weaveffi/blob/main/CHANGELOG.md)
records what shipped.

## Callback interfaces

### Async callback methods (planned)

A callback method that returns a future on the consumer side needs a
completion flowing the other way and a cancellation story when the producer
drops the future. The vtable shape is simple (a completion function and
context per async method, which open vtables can add without a new
revision); the hard part is one producer working the same way whether the
consumer runtime is an event loop, a thread pool, or the single-threaded
Wasm host.

### Off-thread value callbacks in Dart (exploring)

Dart can't run a value-returning callback method synchronously on a thread
that isn't its isolate's, so the Dart binding marks its vtables
thread-affine, and a producer that calls such a method from one of its own
threads gets `-4` (`callback called off its thread`) instead of a result.
That's safe, but it means a Dart callback that returns a value only works
when the producer calls it on the thread that passed it in. Lifting the
restriction needs a way to hop to the isolate and wait for the result.

## Definitions

### Types shared between libraries (exploring)

A library is one namespace, and an IDL can be split across files with
`imports:`, but two libraries can't share a type: each declares its own. A
monorepo wants one library's bindings to reference another's records and
objects. The open questions are how the bindings find each other's types
and how each library's contract covers the shared ones.

### Duration and timestamp primitives (exploring)

Producers pass time as an integer with a documented unit (a Rust producer
can declare a custom type, as the macro tour's `Timeout` does). `duration`
and `timestamp` primitives mapped to each language's types would remove the
ambiguity; the open question is the representation.

### Consumer-side custom types (exploring)

A Rust producer can declare a custom type (`#[weaveffi::custom]`) that
crosses as a builtin, such as a `Uuid` as a `string`, but every binding
still exposes the builtin. Per-language conversion hooks in `weaveffi.toml`
would let a binding expose the language's own type (`java.util.UUID`,
`Foundation.UUID`, `uuid.UUID`) instead; the open questions are how a hook
is declared and how a conversion failure on the consumer side is reported.

### Generic and trait-object interfaces (exploring)

WeaveFFI has a fixed set of generic shapes (`T?`, `[T]`, `{K:V}`, `iter<T>`).
Under discussion are trait-object interfaces (one declared method set with
several producer implementations behind `Arc<dyn Trait>`) and, less likely,
parameterized interfaces monomorphized per instantiation.

## Targets and runtime

### Kotlin Multiplatform (exploring)

The Kotlin target reaches Android and the JVM. A Multiplatform flavor using
Kotlin/Native `cinterop` for iOS and desktop is the natural next step.

### Wasm threads and JSPI (exploring)

On `wasm32-unknown-unknown` futures are polled inline and callbacks fire
only while a call is on the stack. A spawner that schedules on the JS event
loop, JavaScript Promise Integration (so a pending future can suspend the
module until a promise settles), or shared-memory builds with Web Workers
would lift those limits.

### Zero-copy blittable records (exploring)

Every record crosses as a serialized value buffer. A record of fixed-size
scalars could instead cross as a `#[repr(C)]` struct, which matters for hot
paths that move many small records. (Numeric lists already cross as typed
arrays.) The open questions are how a record opts in, and how the contract
and every target's layout checks keep the two sides agreeing on the layout.

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
PowerShell equivalents) and installing each toolchain on Windows runners.

## Toward 1.0

1.0 means the surfaces in [What 1.0 will cover](stability.md#what-10-will-cover)
stop changing without a major release. ABI revision 6 was designed so that
the ABI can keep growing after 1.0 without a new revision: new declarations,
callback methods, and error codes are additive, error domains are open, and
vtables accept a consumer generated before a method was appended. 1.0
needs:

- every Tier 1 target (C, C++, Swift, Kotlin, Python, Node.js, and .NET)
  passing the full conformance matrix on Linux, macOS, and Windows, and the
  Tier 2 targets (Go, Ruby, Dart, and WebAssembly) on at least Linux and
  macOS (see [Target tiers](stability.md#target-tiers));
- the deprecation policy in place;
- a schema migration tool and more than one accepted schema version;
- the known limitations on each generator page resolved or documented as
  permanent;
- a review of every published crate's public API.

## Contributing

Open an issue or discussion on
[GitHub](https://github.com/weavefoundry/weaveffi/issues) if an item here
matters to you or something you need is missing.
