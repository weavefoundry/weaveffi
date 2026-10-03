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
(`async-demo` becomes `AsyncDemo`); set `[generators.swift] module_name` to
override it. The C module is always the Swift module's name with a `C` prefix.
Nothing else is configurable: the C symbol prefix and the native library name
come from the package identity.

## Build and link

The package builds as-is with SwiftPM. Depend on it by path or from a
repository, and tell the linker where `libkvstore` lives:

```swift
dependencies: [.package(path: "generated/swift")],
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
flags. `weaveffi package` bundles the desktop libraries under `lib/<platform>/`
and, when the platforms include an iOS slice, assembles the XCFramework itself
on macOS (see [Packaging](../guides/packaging.md#ios)).

Before the first call, the bindings check that the library implements C ABI
revision 3 and that every top-level module's contract checksum matches the
one they were generated from. A mismatch stops the process with a message
naming the module, since a stale binding would otherwise corrupt memory.

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
| Callback interface | `protocol P: AnyObject, Sendable` | Context and vtable |
| `iter<T>` | generated `Sequence` class | Iterator handle |

Strings cross as pointer and length in both directions, so interior NULs
survive. Value buffers decode in place from the library's memory before it's
released. Floats round-trip bit for bit, including NaN and `-0.0`.

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
    /// takes ownership of it (an object token inside a value buffer).
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
wrapper can't be freed mid-call. Writing an object into a value buffer clones
its reference, and reading one adopts the reference into a new wrapper, so two
wrappers can share one native object. There's no `close()`: release happens
when the last Swift reference goes away.

## Errors

A module's error domain becomes an `enum` conforming to `Error` and
`LocalizedError`, one case per code, carrying the message (the code's
documented message when the library sends none) and any payload fields.
`errorCode` returns the numeric code:

```swift
public enum KitchenErrorsError: Error, LocalizedError, Sendable {
    case notFound(message: String)
    case invalidInput(message: String)
}
```

Throwing wrappers raise that enum for domain codes and
`{Module}RuntimeError` (with `errorCode` and `message`) for runtime failures:
`-1` generic, `-2` panic, `-3` marshalling, and `-4` when a callback
implementation threw. Swift has no unchecked errors, so a function that
doesn't declare `throws` stops the process with `fatalError` when the library
reports a failure, with a message naming the function, code, and message (from
the `events` sample, where a subscriber threw):

```text
Events.publish(topic:text:tags:) failed with code -4: route rejected boom
```

## Async and cancellation

An async function becomes an `async` function over a checked continuation;
the library's completion callback resumes it exactly once, from any thread.
A cancellable function also `throws` and runs inside
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

A callback interface becomes a protocol whose methods are all `throws`:

```swift
public protocol ReadyListener: AnyObject, Sendable {
    /// Fires when an item is ready
    func onReady(code: Int32, msg: String) throws
    /// Receives the item itself and says whether to keep listening
    func onItem(item: Item, gadget: Gadget) throws -> Bool
}
```

Implement it with a class. The library may call it from any thread, which is
why the protocol requires `Sendable`; a class guarding its own state can
declare `@unchecked Sendable`. Passing an implementation retains it until the
library calls the vtable's `free`, and objects passed into a method are
adopted wrappers the implementation may keep. A thrown error is reported to
the library as code `-4` with the error's description; it never unwinds
through C.

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
- Contract mismatches, malformed value buffers, and failures of non-throwing
  calls stop the process instead of throwing.
- Callback methods return `Void` or a direct value (a number, `Bool`, or
  C-style enum); richer returns aren't part of the ABI yet.
- File-scope type names must be unique across modules, since every module's
  types share the Swift module's namespace. A type that shares its name with a
  module namespace is qualified with the Swift module name where needed.
- Interface wrappers aren't `Hashable`, so records that contain objects are
  only `Sendable`.
