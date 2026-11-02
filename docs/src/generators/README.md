# Generators

WeaveFFI ships eleven generators, one per target, and every one implements
the whole IDL surface of schema 0.13.0 on C ABI revision 6: records, C-style
and rich enums, reference-counted objects in every position, callback
interfaces, optionals, lists, maps, open error domains and `throws: any`,
async functions with cancellation, and lazy iterators. Each generator names
its output from the library's [identity](../reference/naming.md), binds to
the native library `{library}`, checks the ABI revision and the contract of
every declaration it was generated with when it loads the library
(reporting a mismatch as a catchable error), and produces a tree under
`{out}/{target}/` that builds with the language's normal tooling.

No target needs compiled glue of its own: each binds the C ABI through its
language's own foreign-function interface, so `weaveffi build` compiles
only the producer's library. The targets that link the library when the
consumer builds them (C, C++, Swift, and Go) call its functions directly, so
a missing declaration fails to link; the others load it at run time, resolve
each function by name, and accept a full path to it in the `{P}_LIBRARY`
environment variable (`KVSTORE_LIBRARY`).

## Tiers

Every target passes the same conformance lanes. **Tier 1** targets (C, C++,
Swift, Kotlin, Python, Node.js, and .NET) carry the stability promise and
get ABI work first. **Tier 2** targets (Go, Ruby, Dart, and WebAssembly) may
lag a release behind on new ABI features. See
[Stability](../stability.md#target-tiers).

## Capability matrix

| Target | Tier | Reaches the C ABI through | Finds the library | A failed load-time check |
|--------|------|---------------------------|-------------------|--------------------------|
| [C](c.md) | 1 | the header `{library}.h` (and `{library}_buffer.h`) | linked when built | `{p}_contract_check()` returns the first mismatch |
| [C++](cpp.md) | 1 | a C++ API over the C header | linked when built | a catchable error |
| [Swift](swift.md) | 1 | a SwiftPM package over the C header | linked when built | a check function that throws |
| [Kotlin](kotlin.md) | 1 | JNA | loaded at run time | the load error |
| [Node.js](node.md) | 1 | koffi | loaded at run time | the load error |
| [Python](python.md) | 1 | `ctypes`, plus libffi closures for callback methods that return a structure | loaded at run time | the import error |
| [.NET](dotnet.md) | 1 | P/Invoke (`[LibraryImport]`) | loaded at run time | the load error |
| [Go](go.md) | 2 | cgo | linked when built | a check function that returns an `error` |
| [Ruby](ruby.md) | 2 | the `ffi` gem | loaded at run time | the load error |
| [Dart](dart.md) | 2 | `dart:ffi`, with [thread-affine](../guides/errors-and-memory.md#thread-affine-callbacks) callback vtables | loaded at run time | the load error |
| [WebAssembly](wasm.md) | 2 | the `wasm32` module's exports | the `.wasm` module, loaded at run time | the load error |

The language pages are authoritative for each target's package layout, type
mapping, names, async and cancellation surface, and toolchain versions.
Notes that apply across targets:

- **Objects.** Every wrapper owns one strong reference and keeps itself
  alive for the duration of each native call, so it can't be released
  mid-call. Disposing twice is a no-op; using a disposed wrapper raises the
  target's usage error.
- **Errors.** A callable that throws a domain raises that domain's typed
  error; a code the bindings don't know (one a newer library added) raises
  the domain's base error with its code and message. A `throws: any`
  callable raises the target's root error type with code `-1`. A non-zero
  code from a callable that declares no errors is a
  [trap](../guides/errors-and-memory.md#the-trap-policy).
- **Callbacks.** Callback methods may be invoked from any producer thread
  (Dart's value-returning methods excepted). An implementation that raises a
  code of the method's own domain reports it with its fields; any other
  failure is reported as `-1` with its message, or as `-4` from a method
  that declares no errors. An object passed to a callback is adopted by the
  implementation. Each binding sets its vtable's `size`, so a library that
  gained callback methods still accepts it.
- **Async.** A cancelled call raises the language's cancellation error. Go
  and Ruby block the calling goroutine or thread while the native work runs
  elsewhere. See [Async and Cancellation](../guides/async.md).

## Fixture compile checks

Snapshot tests pin the generated text; the fixture check proves it's valid
code. `scripts/check-fixtures.sh <target>` generates every fixture in
`crates/weaveffi-cli/tests/fixtures/` (`kitchen_sink`, `edge_cases`,
`imports`, `docs_everywhere`, `shapes`) together with the C header, then
runs `scripts/fixtures/<target>.sh`, which compiles or type-checks the
output with the target's own toolchain. CI runs one job per target. A
missing tool is a printed skip locally and a failure when `CI=true`.

## Conformance lanes

`conformance/run.sh` is the behavioral gate. It builds the three
[sample](../samples.md) producers with leak counting enabled, runs `weaveffi
generate` on each sample crate, reading the API from that same build
(`--library`), and then runs `conformance/<lang>/run.sh` for every language.
Each language declares one lane per sample (`python-calculator`,
`python-codec`, `python-kvstore`, and so on); a lane compiles and runs that
sample's consumer under a timeout. The consumer binds only through the
generated package, asserts concrete results, and checks that every
`{p}_debug_live` counter is zero at exit after forcing its garbage
collector:

- **calculator**: the getting-started surface (a call, typed and untyped
  errors, a string in and out, an optional scalar, and typed arrays).
- **codec**: the shared-vector loop. Fetch each of the producer's test
  vectors, decode it, pass it back to `check_vector`, and echo each
  primitive through the matching family; then check a few vectors built
  from literals.
- **kvstore**: every feature, including callbacks implemented in the
  consumer's language, cancellation, and iterators.

The C lanes also implement a producer by hand (`c-producer-exports`) and
check that it exports every symbol the header requires.

```bash
bash conformance/run.sh                        # every lane
ONLY=python bash conformance/run.sh            # one language
ONLY=c,go-kvstore SKIP_GEN=1 bash conformance/run.sh
```

`SKIP` excludes lanes, `SKIP_GEN=1` reuses previously generated bindings,
and `LANE_TIMEOUT` sets the per-lane limit in seconds (default 300). A
missing toolchain skips its language with a note locally and fails it when
`CI=true`. CI runs every language on Linux and macOS.
