# Roadmap

WeaveFFI is in active `0.x` development. Schema `0.9.0` and ABI revision 2
landed the object model the project had been building toward: reference-counted
interfaces that compose with every other type, callback interfaces the consumer
implements, and a pluggable async executor. This page lists what comes next.
Items are marked **planned** (the design is settled and the work is scheduled)
or **exploring** (we want it, but the design has open questions). Nothing here
carries a date; the [CHANGELOG][changelog] is the source of truth for what has
shipped, and [Stability and Versioning](stability.md) explains how releases and
schema versions work.

## Callback interfaces

### Callback methods returning strings, buffers, and objects (planned)

Today a callback-interface method returns nothing, a scalar, `bool`, or a
C-style enum, because the ABI has no way for a consumer allocation to cross
back into the producer safely: the producer frees with its own allocator, and
eleven languages have eleven allocators. The planned fix is an explicit
allocator contract for callback returns. The consumer will write the result
into producer-owned storage obtained through the runtime (in the spirit of
`weaveffi_alloc`, which the Wasm target already uses), or return a consumer
allocation together with a release function the producer calls once it has
copied the bytes. Once that contract exists, `string`, `bytes`, records, rich
enums, optionals, lists, maps, and objects become valid callback returns on
every target, and `throws` on callback methods (a typed domain error raised by
the consumer, rather than the catch-all `FOREIGN_ERROR_CODE`) follows
naturally.

### Async callback methods (planned)

A callback method that returns a future on the consumer side (`async` in
Swift, Python, Kotlin, and JavaScript; a `Task` in .NET; a `Future` in Dart)
needs a completion callback flowing the other way, plus a cancellation story
when the producer drops the future. The vtable shape is straightforward (an
extra completion function and context per async method); the hard part is
making the same producer code work whether the consumer runtime is an event
loop, a thread pool, or the single-threaded Wasm host. This lands after the
allocator contract above, because an async result needs to carry the same
return types a synchronous one does.

## Type system

### Generic and trait-object interfaces (exploring)

WeaveFFI deliberately ships a fixed set of generic shapes (`T?`, `[T]`,
`{K:V}`, `iter<T>`) rather than user-defined generics. Two extensions are under
discussion. The first is *trait-object interfaces*: an interface declared as a
set of methods with more than one producer-side implementation, so a Rust
`Arc<dyn Trait>` can be returned and each consumer sees one class. The second
is *parameterized interfaces* such as `Cache<K, V>`, monomorphized per
instantiation in the IDL. The first is likely; the second only if it can be
done without every generator growing type-erasure machinery.

### Duration and timestamp primitives (exploring)

Most real APIs carry time. Today producers pass `i64` seconds or milliseconds
and document the unit. A `duration` primitive (nanoseconds as `i64`, mapped to
`Duration`, `TimeInterval`, `TimeSpan`, `timedelta`, `kotlin.time.Duration`,
and so on) and a `timestamp` primitive (a UTC instant) would remove the
ambiguity. The open question is representation: one width for everything, or a
`u64` seconds plus `u32` nanoseconds pair as UniFFI uses.

## IDL

### Multi-file IDL imports (planned)

An API is one document today. Large APIs want to split by module and share
types across files, and a monorepo wants to reference an interface declared by
another package's IDL. The plan is an `imports:` list that resolves relative
paths at parse time, with bare type names remaining unique across the merged
API (as they are today) and `weaveffi diff`, `validate`, and the output cache
tracking every imported file. Annotated Rust already handles this shape
naturally (one crate, many `#[weaveffi::module]` blocks), so the IDL path is
the one catching up.

## Generators and runtime

### Per-language support packages (exploring)

Every generated package inlines its own helper code: the value-buffer codec,
the error hierarchy, the object wrapper base, the callback trampolines. That
keeps consumers free of any WeaveFFI dependency, which is a design principle we
intend to keep. It also means a codec fix ships as a regeneration of every
package. We are exploring extracting the stable runtime portions into optional
per-language support packages (an npm package, a Swift target, a Python wheel,
and so on) that generated code can depend on *if the library author opts in*,
with inlining remaining the default. The conformance harness and the `codec`
sample exist so that this refactor, if it happens, is mechanical.

### Wasm threads and spawner (exploring)

On `wasm32-unknown-unknown` the default spawner drives each future inline,
callback-interface methods fire only while a call into the module is on the
stack, and a producer that calls back from a spawned thread can't run at all.
Two paths are being evaluated: a spawner that schedules futures on the JS
microtask queue through `wasm-bindgen-futures`-style glue, and support for
`wasm32` builds with shared memory and Web Workers (`atomics` plus
`bulk-memory`) where a real thread-per-future spawner works. Either would also
let Emscripten mode regain async functions and callback interfaces, which it
currently rejects at generation time.

### Library naming in bare `generate` output (planned)

`weaveffi package` names the native library after the package identity on
every target. The bare `weaveffi generate` trees are not yet consistent: Dart
loads `lib<package>.<ext>`, while Python, Node (`binding.gyp`), Swift
(`module.modulemap`), and Kotlin (`CMakeLists.txt`) still assume a library
called `weaveffi`, and the bare Kotlin `CMakeLists.txt` doesn't link the JNI
shim against the producer at all (the packaged module does). The plan is to
derive one library base name from the package identity in the binding model
and have every generator use it, so `WEAVEFFI_LIBRARY` overrides stop being
necessary in the tutorials.

### Cancel tokens on wasm (planned)

The wasm glue passes a null cancel token for `#[weaveffi::cancellable]`
functions, so JS can't cancel an in-flight async call the way every native
target can (`AbortSignal` is the natural shape). This follows the spawner
work above, since inline completion leaves nothing to cancel today.

### Kotlin Multiplatform and other targets (exploring)

The Kotlin generator targets Android and the desktop JVM through JNI. A Kotlin
Multiplatform flavor (Kotlin/Native `cinterop` for iOS and desktop) is the
natural next step. Further targets (Java without Kotlin, PHP, Lua) are
possible because a generator is a self-contained crate implementing
`LanguageBackend`, but none is scheduled.

## Toward 1.0

1.0 means the surfaces listed in [What semver covers](stability.md#what-semver-covers-post-10)
stop changing without a major release. We will cut it when all of the following
hold:

- **ABI revision 2 has been stable for several minor releases** with no
  incompatible change, and the callback-return allocator contract has shipped
  (so that 1.0 doesn't immediately need revision 3).
- **Every target passes the full conformance matrix**, including the `codec`
  round-trip lane, the async lanes, and the callback-interface lanes, on
  Linux, macOS, and Windows in CI.
- **Multi-file imports and the deprecation policy are live**, so a post-1.0
  API can grow and shed surface the way the
  [deprecation policy](stability.md#post-10-deprecation-policy) describes.
- **The schema has a migration tool** (`weaveffi migrate`) and
  `SUPPORTED_VERSIONS` accepts more than one version.
- **The public Rust API of every published crate has been reviewed** for
  items that should be `#[doc(hidden)]` or private before they become a
  contract.

Until then, expect pre-1.0 churn to be batched (one schema bump per minor
release) and documented, and pin the CLI version in CI as
[Stability and Versioning](stability.md) recommends.

## Contributing to the roadmap

Open a discussion or issue on
[GitHub](https://github.com/weavefoundry/weaveffi/issues) if one of the
exploring items matters to you, or if something you need isn't listed. Items
move from exploring to planned when a design has been written up and reviewed;
see [CONTRIBUTING.md](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md)
for how proposals work.

[changelog]: https://github.com/weavefoundry/weaveffi/blob/main/CHANGELOG.md
