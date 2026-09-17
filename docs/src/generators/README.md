# Generators

This section contains language-specific generators and guidance for using the
artifacts they produce. Choose a target below to explore the details.

## Feature support matrix

Every generator implements the full IDL surface of schema 0.9.0 on ABI
revision 2: records, plain and rich enums, reference-counted interface
objects (including nullable objects and objects inside records, lists,
optionals, iterators, and async results), callback interfaces,
optionals, lists, maps, typed error domains with opt-in `throws`, and
nested modules, plus the call shapes below. A generator that cannot
support a feature declares it in its `TargetCapabilities`, and
`weaveffi generate` fails loudly when an IDL uses a feature the
selected target cannot deliver (no silent skips).

| Target | Object disposal | Callback interface | Async functions | Iterators (`iter<T>`) | `weaveffi package` bundles |
|--------|-----------------|--------------------|-----------------|-----------------------|----------------------------|
| [C](c.md) | raw `_clone`/`_destroy` symbols | `ctx` + vtable struct with `free` | raw completion-callback ABI | raw `_next`/`_destroy` | every supplied slice (CMake selects desktop) |
| [C++](cpp.md) | copyable RAII class (copy = `_clone`) | abstract class via `std::shared_ptr` | `std::future<T>` | move-only input range | desktop slices |
| [Swift](swift.md) | `final class`, `deinit` releases | protocol | `async throws` | `Sequence` | desktop slices (XCFramework) |
| [Kotlin](kotlin.md) | `AutoCloseable` `close()` + `Cleaner` backstop | `interface` via JNI shim | `suspend fun` | `Iterator<T>` | `jniLibs/<abi>/` + desktop `resources/natives/` |
| [Node.js](node.md) | `close()`/`Symbol.dispose` + `FinalizationRegistry` | plain object (TS `interface`) | `Promise<T>` | `IterableIterator<T>` | desktop slices as `optionalDependencies` |
| [Wasm](wasm.md) | `close()`/`Symbol.dispose` + `FinalizationRegistry` | plain object via table trampolines | `Promise<T>` (settles inline) | `IterableIterator<T>` | the `wasm32` `.wasm` |
| [Python](python.md) | `close()` + `__del__`, context manager | abstract base class | `async def` | iterator | desktop slices (platform wheel) |
| [.NET](dotnet.md) | `IDisposable` + finalizer | C# `interface` | `Task<T>` | `IEnumerable<T>` (single pass) | desktop slices under `runtimes/<rid>/native/` |
| [Dart](dart.md) | `dispose()` + `NativeFinalizer` | abstract class | `Future<T>` | `Iterable<T>` (single pass) | desktop slices |
| [Go](go.md) | `Close()` + `runtime.SetFinalizer` | Go `interface` | blocking bridge | `iter.Seq`/`iter.Seq2` | desktop slices (relocatable cgo preamble) |
| [Ruby](ruby.md) | `close` + `FFI::AutoPointer` | duck-typed object | blocking bridge | `Enumerator` | desktop slices (platform gems) |

Notes:

- **Objects are reference counted.** Every wrapper owns one strong
  reference to the producer's `Arc<T>`; disposal drops it, and copying a
  wrapper or placing it in a record, list, or optional mints a new
  reference through the `_clone` symbol. Use after disposal is a
  consumer error surfaced in the language's idiom (an exception with
  `MARSHAL_ERROR_CODE` `-3`, `ObjectDisposedException`, `StateError`, a
  Go panic, and so on); each page documents its own.
- **Callback implementations own the objects they receive.** An object
  argument to a callback method is adopted by the implementation, which
  disposes it. A callback implementation that throws surfaces to the
  caller of the producer function as `FOREIGN_ERROR_CODE` `-4` through
  the language's error type; a non-throwing callable follows the
  language's trap idiom.
- **Iterators are lazy.** Every target wraps the C ABI's
  handle/`_next`/`_destroy` triple in its native lazy idiom, pulling one
  element per consumer step and destroying the handle exactly once. C
  exposes the raw symbols directly.
- **Go and Ruby async** wrappers block the calling thread until the
  producer's completion callback fires (a channel receive in Go, a
  `Queue#pop` in Ruby). Run them from a goroutine or Ruby thread for
  concurrency; the native producer still runs off-thread.
- **Thread affinity of callbacks differs.** Dart's `isolateLocal`
  callables require the producer to call back synchronously on the
  calling thread; Node blocks the producer thread while the JS thread
  runs the method and can deadlock if a synchronous call from JS waits
  on a worker's callback; Kotlin attaches producer threads to the JVM;
  Ruby and Python rely on their FFI's GVL/GIL dispatch; Wasm is
  single-threaded, so callbacks fire only while a call into the module
  is on the stack ([details](wasm.md#callback-interfaces)). In
  [Emscripten mode](wasm.md#emscripten-mode) callback interfaces and
  async functions are unsupported and, when allowed, become explicit
  throwing stubs rather than silent no-ops.
- **64-bit integers.** Node and Wasm use `bigint`; Dart and Kotlin carry
  `u64` in a signed `int`/`Long` bit pattern; the other targets have
  native 64-bit types. `NaN`, infinities, and `-0` cross every boundary
  unchanged.
