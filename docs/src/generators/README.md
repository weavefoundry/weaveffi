# Generators

This section contains language-specific generators and guidance for using the
artifacts they produce. Choose a target below to explore the details.

## Feature support matrix

Every generator implements the full IDL surface (structs, enums,
interfaces, optionals, lists, maps, typed handles, borrowed parameters,
builders, typed error domains with opt-in `throws`, and nested modules)
plus the call shapes below. A
generator that cannot support a feature declares it in its
`TargetCapabilities`, and `weaveffi generate` fails loudly when an IDL
uses a feature the selected target cannot deliver (no silent skips).

| Target | Async functions | Iterators (`iter<T>`) | Callbacks | Listeners |
|--------|:---:|:---:|:---:|:---:|
| C | ✓ (raw callback ABI) | ✓ | ✓ | ✓ |
| C++ | ✓ (`std::future<T>`) | ✓ | ✓ (`std::function`) | ✓ |
| Swift | ✓ (`async throws`) | ✓ | ✓ (closures) | ✓ |
| Android (Kotlin) | ✓ (`suspend fun`) | ✓ | ✓ (lambdas via JNI) | ✓ |
| Node.js | ✓ (`Promise<T>`) | ✓ | ✓ (thread-safe functions) | ✓ |
| Python | ✓ (`async def`) | ✓ | ✓ (`CFUNCTYPE`) | ✓ |
| .NET | ✓ (`Task<T>`) | ✓ | ✓ (delegates) | ✓ |
| Dart | ✓ (`Future<T>`) | ✓ | ✓ (`NativeCallable`) | ✓ |
| Go | ✓ (blocking bridge) | ✓ | ✓ (exported trampolines) | ✓ |
| Ruby | ✓ (blocking bridge) | ✓ | ✓ (`FFI::Function`) | ✓ |
| Wasm | ✓ (`Promise<T>`) | ✓ | ✓ (table trampolines) | ✓ |

Notes:

- **Iterators are lazy.** Every target wraps the C ABI's
  handle/`_next`/`_destroy` triple in its native lazy idiom (Go
  `iter.Seq`, Swift `Sequence`, C++ input-iterator range, Kotlin
  `Iterator`, JS iterables, Python iterators, .NET `IEnumerable<T>`,
  Dart `Iterable`, Ruby `Enumerator`), pulling one element per
  consumer step and destroying the handle exactly once. C exposes the
  raw symbols directly.
- **Go and Ruby async** wrappers block the calling thread until the
  producer's completion callback fires (a channel receive in Go, a
  `Queue#pop` in Ruby). Run them from a goroutine or Ruby thread for
  concurrency; the native producer still runs off-thread.
- **Wasm callbacks/listeners deliver synchronously.** The loader
  installs one long-lived JavaScript trampoline per callback typedef in
  the module's function table, so the producer's `emit_*` dispatches
  straight back into JS. Because `wasm32-unknown-unknown` is
  single-threaded, events fire only while a call into the module is on
  the stack; a producer that emits from a spawned thread cannot run on
  this target at all ([details](wasm.md#callbacks-and-listeners)). In
  [Emscripten mode](wasm.md#emscripten-mode) callbacks, listeners, and
  async functions become explicit throwing stubs rather than silent
  no-ops.
