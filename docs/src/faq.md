# FAQ

## Why not UniFFI?

[UniFFI](https://mozilla.github.io/uniffi-rs/) is excellent and ships in
production at Mozilla; if you need only Swift, Kotlin, and Python and want
maximum maturity, it's the safer pick. WeaveFFI exists for a different set of
needs: eleven first-party targets from one definition, a producer that can be
written in any language with a C ABI (the generated header is a public
contract, not private scaffolding), a standalone CLI built for CI, and a
schema-checked YAML, JSON, or TOML IDL. UniFFI is still ahead on async
callback methods, custom types, and cross-crate type imports. See the
[comparison](comparison.md) for the full table.

## Can I use it with a C++ codebase?

To *consume* a library from C++, `--target cpp` emits a header-only RAII API
with `std::optional`, `std::vector`, `std::future`, exceptions, and a
`CMakeLists.txt`; see [C++](generators/cpp.md). To *expose* an existing C++
library, describe the surface in an IDL and implement the generated C header
in C++; WeaveFFI doesn't parse C++ headers. For automatic wrapping of C++
headers, look at [autocxx](https://github.com/google/autocxx) or
[SWIG](https://www.swig.org/).

## Does it support generics?

Through built-in shapes rather than user-defined generics: `T?`, `[T]`,
`{K:V}`, and `iter<T>`, which compose with each other and with every other
type, including objects (`[Store]`, `{string:Store}`, `iter<Store>`).
Arbitrary generics would push type erasure into every generator;
trait-object interfaces are on the [roadmap](roadmap.md).

## What's the runtime overhead?

A call costs the marshalling of its arguments, one `extern "C"` call, and the
marshalling of its result. Scalars pass by value. Strings and bytes pass as
borrowed `(ptr, len)` views, so the producer copies only what it keeps.
Objects pass as one pointer. Records and collections are encoded into one
value buffer per value, with single-copy fast paths for byte and numeric
lists. Async calls add a completion callback and whatever executor drives
the future. The runtime itself is the small `weaveffi::abi` module; the
`leak-check` counters cost an atomic update per counted operation and are off
by default.

## How are errors propagated?

Every fallible C call takes a `{prefix}_error*` out-parameter; async calls
receive one in their completion. Positive codes are your declared error
domain, negative codes are the runtime's (generic, panic, marshalling,
callback failure, cancelled). A function marked `throws: true` (a `Result` in
Rust) surfaces domain codes as typed errors in every language; a function
without it traps on failure, since a failure there is a bug. See
[Errors and Memory](guides/errors-and-memory.md#the-trap-policy).

## Can two WeaveFFI libraries live in one process?

Yes. Every symbol, type, macro, package, and environment variable is named
after the library's own prefix and name (`kvstore_error`,
`KVSTORE_LIBRARY`), and none after WeaveFFI, so two libraries never
collide, whether linked statically or loaded dynamically. Each carries its
own copy of the small runtime.

## What happens if the bindings and the library drift apart?

They refuse to load. Every generated consumer compares the library's ABI
revision with the one it was generated against, and checks that every
declaration it uses is in the library's contract table with the same
signature hash. On a mismatch it raises the language's load error naming the
declaration that's missing or changed. The hashes cover names, types, and
flags, but not documentation or declaration order, so editing doc comments
never breaks a deployed binding, and neither does adding a function, method,
or type.
In CI, `weaveffi diff --check` catches stale committed bindings before they
ship.

## Does the CLI parse my Rust source?

No. `#[weaveffi::module]` embeds a description of the API in the library it
compiles, and `weaveffi generate` builds the crate and reads that
description back out of the library, so macros, type aliases, and `#[cfg]`
are already resolved and the bindings match the build exactly. `weaveffi
extract` prints it as an IDL. See [Library Mode](guides/extract.md).

## Can I customize the generated code?

Through `weaveffi.toml`: package metadata, per-target names (the Swift
module, the Kotlin package, the Go module path, each set with `name`), and
target-specific options. Run formatters as a separate step after
`weaveffi generate`. See
[Project Configuration](guides/config.md). Changing the C ABI itself is a
generator contribution; see the [architecture guide](architecture.md).

## Does it work with Flutter?

The Dart target emits `dart:ffi` bindings with a `pubspec.yaml` usable from
Flutter and plain Dart on every platform that supports `dart:ffi`.
`weaveffi package --target dart` bundles the desktop libraries under
`native/<platform>/`; for iOS and Android you add the library to the Flutter
app's native build yourself, since Flutter's native-assets build isn't
generated yet. For the web, use the WebAssembly target. See
[Dart](generators/dart.md).

## Is it Windows-friendly?

The CLI is plain Rust and runs on Windows, and the workspace's tests run on
Windows in CI. Headers carry a `{PREFIX}_API` macro that resolves to
`__declspec(dllimport)` for consumers and `dllexport` when a C or C++
producer defines `{PREFIX}_BUILD`, and every loader knows the `{library}.dll`
naming. The conformance harness runs on Linux and macOS today; Windows lanes
are on the [roadmap](roadmap.md#windows-conformance-lanes-planned).

## How do I distribute the native library?

Build it per platform and let `weaveffi package` bundle it into each
ecosystem's package layout (npm platform packages, platform wheels, NuGet
`runtimes/`, Android `jniLibs/`, and so on), then publish with each
ecosystem's own tools. Every generated manifest takes its name and version
from `[package]`. See [Packaging](guides/packaging.md).

## Who owns an object, and when is it freed?

The producer reference-counts it. Each wrapper holds one strong reference and
releases it through the language's disposal idiom (`close()`, `Dispose()`,
`deinit`, a destructor), with a garbage-collector backstop; the object is
freed when the last reference anywhere goes. Objects returned, yielded,
delivered, or passed to a callback are references the receiver adopts;
objects passed to the producer are borrowed. See
[Errors and Memory](guides/errors-and-memory.md#objects).

## Which executor runs my async functions, and how does cancellation work?

By default, a small pool of worker threads (one per core, at least two)
started on the first call. With the `weaveffi` crate's `tokio` feature it's
Tokio: the current runtime when the call is made from inside one, otherwise
one the library creates. `weaveffi::set_spawner` installs any other executor
and overrides both. Consumers see their native async idiom, and cancelling it (a Swift `Task`, a Kotlin coroutine, an
`AbortSignal`, a `CancellationToken`, a Go `context`) cancels the native
call: the runtime drops the future and completes with the cancelled code,
which surfaces as the language's cancellation error. See
[Async and Cancellation](guides/async.md).

## What's the license?

MIT OR Apache-2.0, at your option. Generated code carries no license header
of its own and is yours to license as you like.
