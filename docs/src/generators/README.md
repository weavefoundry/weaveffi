# Generators

WeaveFFI ships eleven generators, one per target. Every generator implements
the whole IDL surface of schema 0.12.0 on ABI revision 5: records, C-style
and rich enums, reference-counted objects in every position, callback
interfaces, optionals, lists, maps, typed and open error domains (several per
module, plus `throws: any`), nested modules, async functions with
cancellation, and lazy iterators. Optional scalars and numeric lists cross
the ABI directly (the OptDirect and Slice families) rather than through a
value buffer, in every position. Each generator names its output from the
library's [identity](../reference/naming.md), binds to the native library
`{library}`, checks the ABI revision and every top-level module's contract
table at load time (reporting a mismatch as a catchable error), and produces
a tree under `{out}/{target}/` that builds with the language's normal
tooling.

The targets that load the library at run time (Python, Ruby, .NET, Dart,
Kotlin on the JVM, and WebAssembly on Node.js) accept a full path to it in
the `{PREFIX}_LIBRARY` environment variable (`KVSTORE_LIBRARY`). The others
link it when they're built: C, Swift, and Go through the platform linker, C++
through its CMake target (which reads `{PREFIX}_LIBRARY` at configure time),
and the Node.js addon when `npm install` compiles it. Each language page says
where its package looks.

## Tiers

Every target passes the same conformance lanes. **Tier 1** targets (C, C++,
Swift, Kotlin, Python, Node.js, and .NET) carry the stability promise and
get ABI work first. **Tier 2** targets (Go, Ruby, Dart, and WebAssembly) may
lag a release behind on new ABI features. See
[Stability](../stability.md#target-tiers).

## Capability matrix

| Target | Tier | Package / module | Object release | Async | Cancellation | Callback interface | `iter<T>` | Minimum toolchain |
|--------|------|------------------|----------------|-------|--------------|--------------------|-----------|-------------------|
| [C](c.md) | 1 | header `{library}.h` | `_destroy` | launcher plus callback | `{prefix}_cancel_token*` | `ctx` plus vtable struct | `_next`/`_destroy` | C11 |
| [C++](cpp.md) | 1 | header `{library}.hpp`, namespace `{prefix}` | Rule-of-Zero handle; copies clone; `==` is identity | `std::future<T>` | RAII `CancelToken` | abstract class (`shared_ptr`) | `Range<T>` (an input range) | C++17 (clean on C++20), CMake 3.14 |
| [Swift](swift.md) | 1 | module `PascalCase(name)` | `deinit` | `async` / `async throws` | `Task` cancellation | protocol | `NativeSequence<T>` (a `Sequence`) | Swift 5.9 (Swift 5 and 6 language modes) |
| [Kotlin](kotlin.md) | 1 | package `{prefix}` | `close()`, cleaner backstop | `suspend fun` | coroutine cancellation | interface | `NativeIterator<T>` (an `Iterator<T>`) | Kotlin 2.0; Android API 21 or a JVM |
| [Node.js](node.md) | 1 | npm `{name}` (plus `./debug`) | `close()`, `Symbol.dispose`, finalizer | `Promise<T>` | `AbortSignal` | object implementing an interface | `NativeIterator<T>` (`close()`, `Symbol.dispose`) | Node.js 18 |
| [Python](python.md) | 1 | dist `{name}`, import `{prefix}` | `close()`, `with`, finalizer | awaitable | task cancellation | abstract base class | `NativeIterator[T]` (`close()`, `with`) | Python 3.10 |
| [.NET](dotnet.md) | 1 | namespace `PascalCase(name)` | `Dispose()` (`SafeHandle`) | `Task<T>` | `CancellationToken` on every async method | interface `I{Name}` | re-enumerable `IEnumerable<T>` | .NET 8 |
| [Go](go.md) | 2 | module `{name}`, package `{prefix}` without underscores | `Close()`, `runtime.AddCleanup` backstop | blocking call returning `error` | `context.Context` | interface | `iter.Seq` / `iter.Seq2` | Go 1.24 |
| [Ruby](ruby.md) | 2 | gem `{name}`, module `PascalCase(name)` | `close`, `FFI::AutoPointer` | blocking call (only the calling Fiber under a scheduler) | `cancel:` keyword | duck-typed object | `Enumerator` | Ruby 3.2 with the `ffi` gem 1.16 |
| [Dart](dart.md) | 2 | package `{prefix}` (parts under `lib/src/`) | `dispose()`, `NativeFinalizer` | `Future<T>` | cancel-token object | abstract class (thread-affine vtable) | `Iterable<T>` | Dart 3.10 |
| [WebAssembly](wasm.md) | 2 | npm `{name}` (plus `./debug`) | `close()`, `Symbol.dispose`, finalizer | `Promise<T>` | `AbortSignal` | object implementing an interface | `NativeIterator<T>` (`close()`, `Symbol.dispose`) | Node.js 18 or a current browser |

The language pages are authoritative for exact type names, signatures, and
toolchain versions. Notes that apply across targets:

- **Objects.** Every wrapper owns one strong reference and keeps itself
  alive for the duration of each native call, so it can't be released
  mid-call. Disposing twice is a no-op; using a disposed wrapper raises the
  target's usage error.
- **Errors.** A callable that throws a domain raises that domain's typed
  error; a code the bindings don't know (one a newer library added) raises
  the domain's base error with its code and message. A `throws: any`
  callable raises the target's root error type with code `-1`. A non-zero
  code from a callable that declares no errors is a trap.
- **Callbacks.** Callback methods may be invoked from any producer thread
  (Dart's value-returning methods excepted; see its page). An
  implementation that raises a code of the method's own domain reports it
  with its fields; any other failure is reported as `-1` with its message,
  or as `-4` from a method that declares no errors. An object passed to a
  callback is adopted by the implementation.
- **Async.** A cancelled call raises the language's cancellation error. Go
  and Ruby block the calling goroutine or thread while the native work runs
  elsewhere. See [Async and Cancellation](../guides/async.md).
- **Integers.** Every target range-checks integer arguments rather than
  wrapping or truncating them. Node.js and WebAssembly use `bigint` for
  64-bit values; Kotlin uses `ULong` for `u64`; Dart carries `u64` in its
  signed 64-bit `int` (the bits are preserved, so values above `2^63 - 1`
  read as negative). `NaN`, infinities, and `-0` cross unchanged
  everywhere.

## Fixture compile checks

Snapshot tests pin the generated text; the fixture check proves it's valid
code. `scripts/check-fixtures.sh <target>` generates every fixture in
`crates/weaveffi-cli/tests/fixtures/` (`kitchen_sink`, `edge_cases`,
`nested_modules`, `docs_everywhere`, `shapes`) together with the C header,
then runs `scripts/fixtures/<target>.sh`, which compiles or type-checks the
output with the target's own toolchain: `cc` and `c++` with `-Werror` for C,
`clang++ -fsyntax-only` for C++, `swiftc -typecheck`, `kotlinc`, `tsc
--strict` and `node --check`, `py_compile` plus `mypy --strict`,
`dotnet build -warnaserror`, `dart analyze --fatal-infos`, `go vet` and `go
build`, and `ruby -wc`. CI runs one job per target. A missing tool is a
printed skip locally and a failure when `CI=true`.

## Conformance lanes

`conformance/run.sh` is the behavioral gate. It builds the three
[sample](../samples.md) producers with leak counting enabled, runs `weaveffi
generate` on each sample crate, reading the API from that same build
(`--library`), and then runs `conformance/<lang>/run.sh` for every language. Each language declares
exactly three lanes, one per sample (`python-calculator`, `python-codec`,
`python-kvstore`, and so on); a lane compiles and runs that sample's consumer
under a timeout. The consumer binds only through the generated package,
asserts concrete results, and checks that every `{prefix}_debug_live`
counter is zero at exit after forcing its garbage collector:

- **calculator**: the getting-started surface (a call, a typed error, a
  string in and out).
- **codec**: the shared-vector loop. Fetch each of the producer's test
  vectors, decode it, pass it back to `check_vector`, and echo each
  primitive through the direct ABI families; then check a few vectors built
  from literals.
- **kvstore**: every feature, including callbacks implemented in the
  consumer's language, cancellation, and iterators.

The wasm lanes reuse the Node.js consumers, since both targets generate the
same JavaScript API. The C lanes also implement a producer by hand
(`c-producer-exports`) and check its contract table against the header.

```bash
bash conformance/run.sh                        # every lane
ONLY=python bash conformance/run.sh            # one language
ONLY=c,go-kvstore SKIP_GEN=1 bash conformance/run.sh
```

`SKIP` excludes lanes, `SKIP_GEN=1` reuses previously generated bindings,
and `LANE_TIMEOUT` sets the per-lane limit in seconds (default 300). A
missing toolchain skips its language with a note locally and fails it when
`CI=true`. CI runs every
language on Linux and macOS.
