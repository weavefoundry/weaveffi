# Generators

WeaveFFI ships eleven generators, one per target. Every generator implements
the whole IDL surface of schema 0.10.0 on ABI revision 3: records, C-style
and rich enums, reference-counted objects in every position, callback
interfaces, optionals, lists, maps, typed error domains, nested modules,
async functions with cancellation, and lazy iterators. Each one names its
output from the library's [identity](../reference/naming.md), loads
`{library}` (overridable with `{PREFIX}_LIBRARY`), checks the ABI revision
and every module checksum at load time, and produces a tree under
`{out}/{target}/` that builds with the language's normal tooling.

A generator that can't support a feature in some mode (Wasm's Emscripten
mode, for example) declares it, and `weaveffi generate` fails with the
offending declarations listed instead of silently skipping them.

## Capability matrix

| Target | Package / module | Object release | Async | Cancellation | Callback interface | `iter<T>` | Minimum toolchain |
|--------|------------------|----------------|-------|--------------|--------------------|-----------|-------------------|
| [C](c.md) | header `{library}.h` | `_destroy` | launcher plus callback | `{prefix}_cancel_token*` | `ctx` plus vtable struct | `_next`/`_destroy` | C11 |
| [C++](cpp.md) | header `{library}.hpp`, namespace `{prefix}` | RAII destructor; copies clone | `std::future<T>` | RAII `CancelToken` | abstract class | input range | C++17 |
| [Swift](swift.md) | module `PascalCase(name)` | `deinit` | `async` / `async throws` | `Task` cancellation | protocol | `Sequence` | Swift 5.7 |
| [Kotlin](kotlin.md) | package `{prefix}` | `close()`, cleaner backstop | `suspend fun` | coroutine cancellation | interface | `Iterator<T>` | Kotlin 1.9; Android API 21 or a JVM |
| [Node.js](node.md) | npm `{name}` | `close()`, `Symbol.dispose`, finalizer | `Promise<T>` | `AbortSignal` | object implementing an interface | `IterableIterator<T>` | Node.js 18 |
| [WebAssembly](wasm.md) | npm `{name}` | `close()`, `Symbol.dispose`, finalizer | `Promise<T>` | `AbortSignal` | object implementing an interface | `IterableIterator<T>` | Node.js 18 or a current browser |
| [Python](python.md) | dist `{name}`, import `{prefix}` | `close()`, `with`, finalizer | awaitable | task cancellation | abstract base class | iterator | Python 3.9 |
| [.NET](dotnet.md) | namespace `PascalCase(name)` | `Dispose()` (`SafeHandle`) | `Task<T>` | `CancellationToken` | interface | `IEnumerable<T>` | .NET 8 |
| [Dart](dart.md) | package `{prefix}` | `dispose()`, `NativeFinalizer` | `Future<T>` | cancel-token object | abstract class | `Iterable<T>` | Dart 3.1 |
| [Go](go.md) | module `{name}`, package `{prefix}` | `Close()`, finalizer | blocking call | `context.Context` | interface | `iter.Seq` / `iter.Seq2` | Go 1.23 |
| [Ruby](ruby.md) | gem `{name}`, module `PascalCase(name)` | `close`, `FFI::AutoPointer` | blocking call | `cancel:` keyword | duck-typed object | `Enumerator` | Ruby with the `ffi` gem 1.15 |

The language pages are authoritative for exact type names, signatures, and
toolchain versions. Notes that apply across targets:

- **Objects.** Every wrapper owns one strong reference and keeps itself
  alive for the duration of each native call, so it can't be released
  mid-call. Disposing twice is a no-op; using a disposed wrapper raises the
  target's usage error.
- **Callbacks.** Callback methods may be invoked from any producer thread.
  An implementation that throws reaches the original caller as runtime code
  `-4`; an object passed to a callback is adopted by the implementation.
- **Async.** A cancelled call raises the language's cancellation error. Go
  and Ruby block the calling goroutine or thread while the native work runs
  elsewhere. See [Async and Cancellation](../guides/async.md).
- **64-bit integers.** Node.js and WebAssembly use `bigint`; Kotlin and Dart
  carry `u64` in a signed 64-bit type. `NaN`, infinities, and `-0` cross
  unchanged everywhere.

## Fixture compile checks

Snapshot tests pin the generated text; the fixture check proves it's valid
code. `scripts/check-fixtures.sh <target>` generates every fixture in
`crates/weaveffi-cli/tests/fixtures/` (`kitchen_sink`, `edge_cases`,
`nested_modules`, `docs_everywhere`, `shapes`) together with the C header,
then runs `scripts/fixtures/<target>.sh`, which compiles or type-checks the
output with the target's own toolchain: `cc` and `c++` with `-Werror` for C,
`clang++ -fsyntax-only` for C++, `swiftc -typecheck`, `kotlinc`, `tsc
--strict` and `node --check`, `py_compile` plus a type checker on the stubs,
`dotnet build -warnaserror`, `dart analyze --fatal-infos`, `go vet` and `go
build`, and `ruby -wc`. CI runs one job per target.

## Conformance lanes

`conformance/run.sh` is the behavioral gate. It builds the six sample
producers with leak counting enabled, runs `weaveffi generate` on each
sample's `src/lib.rs`, and then runs `conformance/<lang>/run.sh` for every
language. Each lane (`python-kvstore`, `go-events`, and so on) compiles and
runs one consumer against one sample under a timeout; the consumer binds
only through the generated package, asserts concrete results, and checks
that every `{prefix}_debug_live` counter is zero at exit after forcing its
garbage collector. The C lanes also implement a producer by hand
(`c-producer-exports`) and check each checksum against the header.

```bash
bash conformance/run.sh                        # every lane
ONLY=python bash conformance/run.sh            # one language
ONLY=c,go-kvstore SKIP_GEN=1 bash conformance/run.sh
```

`SKIP` excludes lanes, `SKIP_GEN=1` reuses previously generated bindings,
and `LANE_TIMEOUT` sets the per-lane limit in seconds (default 300). A
missing toolchain fails its lanes; skip them explicitly. CI runs every
language on Linux and macOS.
