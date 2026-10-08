# Swift

The Swift target emits a standalone SwiftPM package that wraps the C ABI in
idiomatic Swift: `throws` for declared errors, plain `Hashable` structs and
enums for values, ARC-managed `final class` wrappers for objects, class-bound
protocols for callback interfaces, `async` functions with task cancellation
for async callables, and lazy `Sequence`s for iterators.

## What gets generated

For a package named `kvstore`:

```text
swift/
  Package.swift
  Sources/
    CKvstore/
      kvstore.h          # copy of the C header
      module.modulemap   # system library: links `kvstore`
    Kvstore/
      Kvstore.swift      # runtime support and the wrapper API
```

The package, product, and Swift module are the package name in PascalCase
(`key-store` becomes `KeyStore`); set `[generators.swift] name` to
override it. The C module is always the Swift module's name with a `C` prefix.
The C symbol prefix and the native library name come from the package
identity.

Types (records, enums, error domains, interfaces, callback protocols, and
iterator classes) sit at file scope, since type names are unique across an
API. Each IDL module's free functions become `static func`s of a caseless
namespace `enum` named after the module in PascalCase, a submodule's enum
nests inside its parent's, and the module's doc comment documents its enum:

```swift
/// An embedded key-value store with listeners, policies, read-through
/// loaders, iteration, and async compaction.
public enum Kv {
    public static func openStore(path: String) async throws -> Store { ... }

    /// Aggregate statistics, namespaced under `kv.stats`: ...
    public enum Stats {
        public static func summarize(store: Store, prefix: String?) throws -> Kvstore.Stats { ... }
    }
}
```

Parameters, methods, record fields, and enum cases are lowerCamelCase
(`expiresAt`, `.keyNotFound`), and every argument is labeled
(`Kv.Stats.summarize(store: store, prefix: nil)`).

| `[generators.swift]` key | Default | Meaning |
|---|---|---|
| `name` | `PascalCase(name)` | Package, product, and module name |
| `min_macos`, `min_ios` | `"11.0"`, `"13.0"` | The manifest's `platforms:` |
| `xcframework_url` | a placeholder | Where the packaged binary target downloads `C{Module}.xcframework.zip`; `{version}` and `{file}` are substituted |

## Build and link

The package builds as-is with SwiftPM (its manifest declares
`swift-tools-version:5.9`). Depend on it by path or from a repository, and
tell the linker where `libkvstore` lives:

```swift
dependencies: [.package(path: "bindings/swift")],
targets: [
    .executableTarget(name: "App", dependencies: [.product(name: "Kvstore", package: "swift")]),
]
```

```bash
swift build -Xlinker -L/path/to/lib
```

Swift binds the library at link time, so at run time the dynamic loader finds
it the usual way (its install name, an rpath, or `DYLD_LIBRARY_PATH` and
`LD_LIBRARY_PATH`). The `{PREFIX}_LIBRARY` variable other targets honor
doesn't apply.

When a `CKvstore.xcframework` sits next to `Package.swift`, the manifest uses
it as a binary target instead of the system library, so apps link without
flags.

`weaveffi package` (on macOS) fuses the static libraries of every Apple
platform built (macOS, iOS, the iOS simulator) into
`CKvstore.xcframework.zip`, writes its SHA-256 checksum, and writes the
package `swift/Kvstore/`, whose manifest declares `CKvstore` as
`.binaryTarget(url:checksum:)` at `xcframework_url`. Upload the archive there
and publish the package; apps on any Apple platform then link with no flags
(see [Packaging](../guides/packaging.md#swift-and-the-xcframework)).

## Load-time checks

Before the first call (the first constructor, static, or free function), the
bindings check, once per process, that the library implements C ABI
revision 4 and that every declaration they were generated with is in its
top-level module's [contract table](../reference/abi.md#load-time-checks)
with the same signature. The expected entries are embedded in the wrapper:

```swift
let wvContract: Void = {
    wvCheckAbiVersion()
    wvCheckContract(kvstore_kv_contract, [
        (0x0969575bfbb012d7, 0xebd38766e3532c4f, "kv.Store.fork"),
        // ...one entry per declaration in `kv` and its submodules
    ])
    wvCheckContract(kvstore_report_contract, [
        // ...
    ])
}()
```

Declarations the library adds are fine. A missing or changed one stops the
process with a message naming it, since a stale binding would otherwise
corrupt memory:

```text
Kvstore: kv.Store.put is missing from the library 'kvstore'; regenerate the bindings or rebuild the library
Kvstore: kv.Store.put changed since these bindings were generated; regenerate the bindings or rebuild the library 'kvstore'
```

## Type mapping

| IDL type | Swift type | At the ABI |
|---|---|---|
| `i8` to `i64`, `u8` to `u64` | `Int8` to `Int64`, `UInt8` to `UInt64` | Direct |
| `f32`, `f64` | `Float`, `Double` | Direct |
| `bool` | `Bool` | C `bool` |
| `string` | `String` | UTF-8 pointer and length |
| `bytes` | `Data` | Pointer and length |
| C-style enum | `enum E: Int32, Sendable` | `int32_t` |
| Record | `struct`, `Sendable` (and `Hashable` when every field is) | Value buffer |
| Rich enum | `enum` with associated values | Value buffer |
| `T?`, `[T]`, `{K: V}` | `T?`, `[T]`, `[K: V]` | Value buffer |
| Interface | `final class`, `@unchecked Sendable` | Object pointer |
| `Interface?` | optional wrapper | Nullable pointer |
| Callback interface | `any P`, where `protocol P: AnyObject, Sendable` | Context and vtable |
| `Callback?` | `(any P)?` | Context and nullable vtable |
| `iter<T>` | generated `Sequence` class | Iterator handle |

Strings cross as pointer and length in both directions, so interior NULs
survive. Value buffers decode in place from the library's memory before it's
released. Floats round-trip bit for bit, including NaN and `-0.0`.

Every type that crosses inside a value buffer conforms to the internal
`WvCodable` protocol: the primitives in the runtime, `Optional`, `Array`, and
`Dictionary` generically, and each record, enum, and interface where it's
declared. A composite type such as `[String: [Entry?]]` therefore has one
encoder and one decoder, the generic ones, rather than a loop at each use:

```swift
extension Entry: WvCodable {
    static func wvRead(_ r: inout WvReader) -> Entry {
        Entry(key: r.read(), value: r.read(), kind: r.read(), version: r.read(), expiresAt: r.read(), tags: r.read(), metadata: r.read())
    }

    func wvWrite(_ w: inout WvWriter) {
        w.write(self.key)
        w.write(self.value)
        // ...
    }
}
```

## Objects and lifetime

An interface becomes a `final class` holding one strong reference. `deinit`
releases it, a constructor named `new` becomes `init`, and other constructors
and statics become `static func`s:

```swift
public final class Gadget: @unchecked Sendable {
    let ptr: OpaquePointer

    init(ptr: OpaquePointer) {
        self.ptr = ptr
    }

    deinit {
        kitchen_sink_kitchen_Gadget_destroy(ptr)
    }

    /// Returns a new strong reference to the same object, for a position that
    /// takes ownership of it.
    func clonePtr() -> OpaquePointer {
        wvNonNull(kitchen_sink_kitchen_Gadget_clone(ptr))
    }

    /// Create a gadget with the given id
    public init(id: Int64) {
        wvLoad()
        var err = WvError()
        let rv = kitchen_sink_kitchen_Gadget_new(id, &err)
        wvTrap(&err)
        self.ptr = wvNonNull(rv)
    }

    /// Render the gadget as a human-readable string
    public func describe() -> String {
        var err = WvError()
        var outLen = 0
        let rv = kitchen_sink_kitchen_Gadget_describe(ptr, &outLen, &err)
        wvTrap(&err)
        return wvTakeString(rv, outLen)
    }
}
```

Swift keeps every argument and the receiver alive for the whole call, so a
wrapper can't be freed mid-call. Writing an object into a value buffer or
returning it from a callback method clones its reference, and reading one
adopts the reference into a new wrapper, so two wrappers can share one native
object. There's no `close()`: release happens when the last Swift reference
goes away. An interface member spelled like one the class declares (`ptr`,
`clonePtr`, `wvRead`, `wvWrite`) gains a trailing `_` ([reserved member
names](../reference/naming.md#identifiers-in-generated-code)).

## Errors

A module's error domain becomes an `enum` conforming to `Error`,
`LocalizedError`, and `Sendable`, named after the domain with an `Error`
suffix unless it already has one (`KvError` stays `KvError`; `KitchenErrors`
becomes `KitchenErrorsError`). It has one case per code, carrying the message
(the code's documented message when the library sends none) and any payload
fields. `errorCode` returns the numeric code:

```swift
public enum KvError: Error, LocalizedError, Sendable {
    case keyNotFound(message: String, key: String)
    case expired(message: String, key: String, expiredAt: Int64)
    case invalidPath(message: String)
    // ...
}
```

A function declared `throws` raises that enum for domain codes and
`{SwiftModule}RuntimeError` (`KvstoreRuntimeError`, with `errorCode` and
`message`) for unknown codes and runtime failures:
`-1` generic, `-2` panic, `-3` marshalling, and `-4` when a callback
implementation failed. Swift has no unchecked errors, so a function that
doesn't declare `throws` follows the [trap policy](../guides/errors-and-memory.md#the-trap-policy):
it stops the process with `fatalError` when the library reports a failure,
naming the function, the code, and the message:

```text
Kvstore.count() failed with code -2: <the producer's panic message>
```

## Async and cancellation

An async function becomes an `async` function over a checked continuation;
the library's completion callback resumes it exactly once, from any thread.
It's marked `throws` only when the IDL function declares errors or is
cancellable; otherwise a failure follows the trap policy
(`Kv.Stats.summarizeAll(stores:)` is `async -> Kvstore.Stats`). A
cancellable function runs inside
`withTaskCancellationHandler`: it creates a native cancel token, passes it to
the launch, and cancels it when the calling task is cancelled. A cancelled
call (code `-5`) throws `CancellationError`, even when the task was cancelled
before the call started:

```swift
public static func doCancellable(input: String) async throws -> String {
    wvLoad()
    let token = WvCancelToken()
    return try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
            let context = Unmanaged.passRetained(WvContinuation(continuation)).toOpaque()
            wvWithUTF8(input) { input_ptr, input_len in
                kitchen_sink_kitchen_do_cancellable(input_ptr, input_len, token.raw, { context, err, resultPtr, resultLen in
                    let cont = Unmanaged<WvContinuation<String, Error>>.fromOpaque(context!).takeRetainedValue().value
                    if let err = err {
                        cont.resume(throwing: wvTakeError(err, wvCancelledOrTrap))
                        return
                    }
                    cont.resume(returning: wvTakeString(resultPtr, resultLen))
                }, context)
            }
        }
    } onCancel: {
        token.cancel()
    }
}
```

The token is released when the wrapper returns; the library holds its own
reference for the call's duration.

## Callback interfaces

A callback interface becomes a class-bound protocol. Every requirement is
`throws`, so any implementation can fail the library's call in progress:

```swift
public protocol Loader: AnyObject, Sendable {
    /// The loader's name, recorded in a loaded entry's metadata under
    /// `source`.
    func name() throws -> String

    /// A store that may already hold `key`, consulted first, if any.
    func fallback(key: String) throws -> Store?

    /// The value for `key`. Fail with `KeyNotFound` naming
    /// `key` when there's none.
    ///
    /// - Throws: ``KvError`` to report a declared failure with its fields, or any other error to fail the call.
    func load(key: String) throws -> Data
}
```

Implement it with a class and pass an instance (`any Loader`) where the API
expects one. The library may call it from any thread, which is why the
protocol requires `Sendable`; a class guarding its own state can declare
`@unchecked Sendable`. Passing an implementation retains it until the library
calls the vtable's `free`, which may happen on any library thread, and
releases it then. An optional parameter (`loader: (any Loader)?`) passes a
null vtable for `nil`.

Arguments and returns follow the [ABI](../reference/abi.md#callback-interfaces):

- Strings, bytes, and value buffers arrive as copies; an object argument
  arrives as a wrapper that adopted the reference the library passed, which
  the implementation may keep.
- A returned number, `Bool`, or C-style enum crosses by value; a returned
  object (`Store` or `Store?`) as a fresh reference the library adopts; and a
  returned string, `Data`, or value buffer (a record, a rich enum, an
  optional, a list, or a map) as a run allocated with `{prefix}_alloc`, which
  the library adopts.
- A requirement declared `throws` in the IDL may throw the module's error
  enum: the library receives its code, its message, and its fields as the
  payload, exactly as if the producer had raised it. Any other error, and
  any error from a requirement not declared `throws`, reaches the library as
  a callback failure (`-4`) with the error's `localizedDescription`. Nothing
  unwinds through C.

```swift
final class FileLoader: Loader, @unchecked Sendable {
    func name() throws -> String { "files" }

    func fallback(key: String) throws -> Store? { nil }

    func load(key: String) throws -> Data {
        guard let data = FileManager.default.contents(atPath: "/data/\(key)") else {
            throw KvError.keyNotFound(message: "no file for \(key)", key: key)
        }
        return data
    }
}

let entry = try store.getOrLoad(key: "settings", loader: FileLoader())
```

## Iterators

An `iter<T>` return becomes a class conforming to `Sequence` and
`IteratorProtocol`. Each `next()` pulls exactly one element from the library;
the native iterator is destroyed on exhaustion or from `deinit` when iteration
stops early. `next()` can't throw, so for a throwing function an error
reported mid-stream ends iteration and is stored in the sequence's `error`
property.

## Known limitations

- The library is linked at build time, so `{PREFIX}_LIBRARY` isn't honored and
  library paths come from the linker and loader.
- Load-time check failures, malformed value buffers, and failures of
  non-throwing calls stop the process instead of throwing.
- A type that shares its name with a module's namespace `enum` (the `Stats`
  record beside the `kv.stats` module's `Kv.Stats`) is qualified with the
  Swift module name where the two would clash (`Kvstore.Stats`).
- Interface wrappers aren't `Hashable`, so records that contain objects are
  only `Sendable`.
