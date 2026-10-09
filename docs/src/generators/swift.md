# Swift

The Swift target emits a standalone SwiftPM package that wraps the
[C ABI](../reference/abi.md) (revision 5) in idiomatic Swift: `throws` with
typed `Error` enums for declared errors, plain `Hashable` structs and enums
for values, ARC-managed `final class` wrappers for objects, class-bound
protocols for callback interfaces, `async` functions with task cancellation
for async callables, and lazy `Sequence`s for iterators.

Swift is a [Tier 1](../stability.md#target-tiers) target: it tracks every
ABI revision as it lands and runs the full conformance suite in CI.

## What gets generated

For a package named `kvstore`:

```text
swift/
  Package.swift
  Sources/
    CKvstore/
      kvstore.h              # copy of the C header
      module.modulemap       # system library: links `kvstore`
    Kvstore/
      Kvstore.swift          # the wrapper API
      WeaveFFIRuntime.swift  # runtime support: load check, codec, errors
```

The package, product, and Swift module are the package name in PascalCase
(`key-store` becomes `KeyStore`); set `[generators.swift] name` to
override it. The C module is always the Swift module's name with a `C` prefix.
The C symbol prefix and the native library name come from the package
identity. `WeaveFFIRuntime.swift` depends only on those names, so it changes
only when they do.

Types (records, enums, error domains, interfaces, and callback protocols)
sit at file scope, since type names are unique across an API. Each IDL
module's free functions become `static func`s of a caseless namespace `enum`
named after the module in PascalCase, a submodule's enum nests inside its
parent's, and the module's doc comment documents its enum:

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
(`Kv.Stats.summarize(store: store, prefix: nil)`). Doc comments carry over,
with backticked API names in their Swift spelling (`` `new_op` `` becomes
`` `newOp` ``), and deprecated declarations get
`@available(*, deprecated, message:)`.

| `[generators.swift]` key | Default | Meaning |
|---|---|---|
| `name` | `PascalCase(name)` | Package, product, and module name |
| `min_macos`, `min_ios` | `"11.0"`, `"13.0"` | The manifest's `platforms:` |
| `xcframework_url` | a placeholder | Where the packaged binary target downloads `C{Module}.xcframework.zip`; `{version}` and `{file}` are substituted |

## Build and link

The package builds as-is with SwiftPM 5.9 or later (its manifest declares
`swift-tools-version:5.9`, so it compiles in the Swift 5 language mode); the
sources also compile without warnings in the Swift 6 language mode, with
complete concurrency checking. Depend on it by path or from a repository,
and tell the linker where `libkvstore` lives:

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
revision 5 and that every declaration they were generated with is in its
top-level module's [contract table](../reference/abi.md#load-time-checks)
with the same signature. The expected rows are embedded in the wrapper, one
per function, member, type, callback method, error domain, and error code:

```swift
func wvCheckContracts() -> KvstoreLibrary.LoadError? {
    if let failure = wvCheckContract(kvstore_kv_contract, [
        (0x0969575bfbb012d7, 0xebd38766e3532c4f, "kv.Store.fork"),
        // ...one row per declaration in `kv` and its submodules
    ]) {
        return failure
    }
    return wvCheckContract(kvstore_report_contract, [
        // ...
    ])
}
```

Declarations, error codes, and callback methods the library adds are fine.
To handle a mismatch, call `{Module}Library.check()` at startup. It runs the
checks (or returns their cached result) and throws a `LoadError` naming the
first mismatch:

```swift
do {
    try KvstoreLibrary.check()
} catch let error as KvstoreLibrary.LoadError {
    // .abiMismatch(found:expected:), .missing(declaration:), or .changed(declaration:)
    print(error.localizedDescription)
}
```

```text
Kvstore: kv.Store.put is missing from the library 'kvstore'; regenerate the bindings or rebuild the library
Kvstore: kv.Store.put changed since these bindings were generated; regenerate the bindings or rebuild the library 'kvstore'
```

A call made while the library doesn't match stops the process with the same
message, whether or not `check()` ran, since a stale binding would otherwise
corrupt memory. `KvstoreLibrary.abiVersion` is the revision the bindings
need.

## Type mapping

| IDL type | Swift type | At the ABI |
|---|---|---|
| `i8` to `i64`, `u8` to `u64` | `Int8` to `Int64`, `UInt8` to `UInt64` | Direct |
| `f32`, `f64` | `Float`, `Double` | Direct |
| `bool` | `Bool` | C `bool` |
| `string` | `String` | UTF-8 pointer and length |
| `bytes` | `Data` | Pointer and length |
| C-style enum | `enum E: Int32, CaseIterable, Sendable` | `int32_t` |
| Record | `struct`, `Hashable` and `Sendable` | Value buffer |
| Rich enum | `enum` with associated values, `Hashable` and `Sendable` | Value buffer |
| `T?` of a number, `bool`, or C-style enum | `T?` | Presence flag and value |
| `[T]` of a number other than `u8` | `[T]` | Typed array: pointer and element count |
| Any other `T?`, `[T]`, `{K: V}` | `T?`, `[T]`, `[K: V]` | Value buffer |
| Interface | `final class`, `Hashable` by identity, `@unchecked Sendable` | Object pointer |
| `Interface?` | optional wrapper | Nullable pointer |
| Callback interface | `any P`, where `protocol P: AnyObject, Sendable` | Context and vtable |
| `Callback?` | `(any P)?` | Context and nullable vtable |
| `iter<T>` | `NativeSequence<T>` | Iterator handle |

An optional scalar (`Int64?`, `Bool?`, `Priority?`) crosses as a flag and a
value in every position, never through a buffer. A numeric array parameter
lends the array's own storage for the call (`withUnsafeBufferPointer`, no
copy); a returned array is copied once out of the library's run, which is
then released. Strings cross as pointer and length in both directions, so
interior NULs survive. Value buffers decode in place from the library's
memory before it's released. Floats round-trip bit for bit, including NaN
and `-0.0`. `usize` and `isize` in a Rust producer are `u64` and `i64`
(`UInt64`, `Int64`); a `char` and a custom type cross as their IDL type
(usually `String`).

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
public final class Gadget: WvObject, Hashable, @unchecked Sendable {
    let ptr: OpaquePointer

    init(ptr: OpaquePointer) {
        self.ptr = ptr
    }

    deinit {
        kitchen_sink_kitchen_Gadget_destroy(ptr)
    }

    func clonePtr() -> OpaquePointer {
        wvNonNull(kitchen_sink_kitchen_Gadget_clone(ptr))
    }

    /// Whether two wrappers hold the same native object.
    public static func == (lhs: Gadget, rhs: Gadget) -> Bool {
        lhs.ptr == rhs.ptr
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(ptr)
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
        let rv = kitchen_sink_kitchen_Gadget_describe(self.ptr, &outLen, &err)
        wvTrap(&err)
        return wvTakeString(rv, outLen)
    }
}
```

Swift keeps every argument and the receiver alive for the whole call, so a
wrapper can't be freed mid-call. Writing an object into a value buffer or
returning it from a callback method clones its reference, and reading one
adopts the reference into a new wrapper, so two wrappers can share one native
object. Wrappers are `Equatable` and `Hashable` by that native identity: two
wrappers are `==` exactly when they hold the same object (`_clone` keeps the
pointer value), whatever its state. So a record or enum carrying objects is
`Hashable` too, and `store.share() == store`. There's no `close()`: release
happens when the last Swift reference goes away. An interface member spelled
like one the class declares (`ptr`, `clonePtr`, `wvRead`, `wvWrite`) gains a
trailing `_` ([reserved member
names](../reference/naming.md#identifiers-in-generated-code)).

## Errors

Each error domain becomes an `enum` conforming to `Error`, `LocalizedError`,
`Hashable`, and `Sendable`, named through the shared naming rule: the
domain's name with one `Error` suffix (`KvError` stays `KvError`,
`KitchenErrors` becomes `KitchenError`, `Failure` becomes `FailureError`). A
module may declare several. Each code is a case carrying the message (the
code's documented message when the library sends none) and the code's
fields; `errorCode` and `message` read them back:

```swift
public enum KvError: Error, LocalizedError, Hashable, Sendable {
    /// key not found
    case keyNotFound(message: String, key: String)
    /// entry expired
    case expired(message: String, key: String, expiredAt: Int64)
    // ...
    /// A code these bindings don't declare, from a newer library.
    case unknown(code: Int32, message: String)
}
```

Domains are open: a code the producer added after the bindings were
generated arrives as `.unknown` with its code and message, never as a crash,
so a `switch` over a domain needs that case (or a `default`). A field named
`message` gains a trailing `_` (`case callbackFailed(message: String,
message_: String)`), and so does a code spelled like a member of the enum
(`errorCode`, `message`, `errorDescription`) or like `unknown` (the
catch-all case then becomes `unknown_`).

A function declared `throws: KvError` raises that enum for the domain's codes
and `{SwiftModule}RuntimeError` (`KvstoreRuntimeError`, with `errorCode` and
`message`) for runtime failures: `-2` panic, `-3` marshalling, and `-4` when a
callback implementation failed. A function declared `throws: any` raises the
runtime error with code `-1` (`KvstoreRuntimeError.untypedCode`) and the
producer's message:

```swift
do {
    _ = try store.importLines(text: "c=3\nbroken")
} catch let error as KvstoreRuntimeError {
    print(error.errorCode, error.message)  // -1 line 2: expected key=value
}
```

Swift has no unchecked errors, so a function that doesn't declare `throws`
follows the [trap policy](../guides/errors-and-memory.md#the-trap-policy):
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
(`Kv.Stats.summarizeAll(stores:)` is `async -> Kvstore.Stats`). Optional
scalars and numeric arrays arrive directly here too
(`versionOf(key:) async -> UInt32?`, `versions(keys:) async -> [UInt32]`). A
cancellable function runs inside `withTaskCancellationHandler`: it creates a
native cancel token, passes it to the launch, and cancels it when the calling
task is cancelled. A cancelled call (code `-5`) throws `CancellationError`,
even when the task was cancelled before the call started:

```swift
public static func doCancellable(input: String) async throws -> String {
    wvLoad()
    let token = WvCancelToken()
    return try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
            let context = Unmanaged.passRetained(WvContinuation(continuation)).toOpaque()
            wvWithUTF8(input) { input_ptr, input_len in
                kitchen_sink_kitchen_do_cancellable(input_ptr, input_len, token.raw, { context, err, result_ptr, result_len in
                    let cont = Unmanaged<WvContinuation<String, Error>>.fromOpaque(context!).takeRetainedValue().value
                    if let err = err {
                        cont.resume(throwing: wvTakeCancellation(err))
                        return
                    }
                    cont.resume(returning: wvTakeString(result_ptr, result_len))
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

A callback interface becomes a class-bound protocol. A requirement is
`throws` when the IDL method declares errors (`throws: Domain` or
`throws: any`), and plain otherwise:

```swift
public protocol Loader: AnyObject, Sendable {
    /// The loader's name, recorded in a loaded entry's metadata under
    /// `source`.
    ///
    /// - Throws: Any error, which fails the library's call with the error's
    ///   description.
    func name() throws -> String

    /// A store that may already hold `key`, consulted first, if any.
    ///
    /// - Throws: Any error, which fails the library's call with the error's
    ///   description.
    func fallback(key: String) throws -> Store?

    /// The value for `key`. Fail with `keyNotFound` naming
    /// `key` when there's none.
    ///
    /// - Throws: ``KvError`` to fail the library's call with that error and
    ///   its fields; any other error fails it with the error's description.
    func load(key: String) throws -> Data
}
```

Implement it with a class and pass an instance (`any Loader`) where the API
expects one. The library may call it from any thread, which is why the
protocol requires `Sendable`; a class guarding its own state can declare
`@unchecked Sendable`. (The Swift vtables leave the thread-affine flag clear.)
Passing an implementation retains it until the library calls the vtable's
`free`, which may happen on any library thread, and releases it then. An
optional parameter (`loader: (any Loader)?`) passes a null vtable for `nil`.

Arguments and returns follow the [ABI](../reference/abi.md#callback-interfaces):

- Strings, bytes, numeric arrays, and value buffers arrive as copies; an
  optional scalar arrives as `T?`; an object argument arrives as a wrapper
  that adopted the reference the library passed, which the implementation
  may keep.
- A returned number, `Bool`, or C-style enum crosses by value, and an
  optional one as a presence flag plus the value; a returned object (`Store`
  or `Store?`) as a fresh reference the library adopts; and a returned
  string, `Data`, numeric array, or value buffer (a record, a rich enum, an
  optional, a list, or a map) as a run allocated with `{prefix}_alloc`,
  which the library adopts.
- A requirement declared `throws: KvError` may throw that enum: the library
  receives its code, message, and fields as the payload, exactly as if the
  producer had raised it (a Rust producer then renders the message from the
  fields; a thrown `.unknown` sends its own code, which the producer treats
  as any other failure). Any other error, from any throwing requirement,
  reaches the library as an untyped failure (`-1`) with the error's
  `errorDescription` (or its `String(describing:)`). Nothing unwinds
  through C.

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

An `iter<T>` return is a `NativeSequence<T>`, one generic class (a
`Sequence` and its own `IteratorProtocol`) for every iterator in the API.
Each `next()` pulls exactly one element from the library; the native
iterator is released as soon as the stream ends, or from `deinit` when
iteration stops early. The sequence is single-pass. `next()` can't throw, so
for a throwing function an error reported mid-stream ends iteration and is
kept in the sequence's `error` property; `collect()` pulls the rest and
throws it instead:

```swift
for key in try store.keys(prefix: "user.") { print(key) }

let expirations: [Int64?] = try store.expirations().collect()
```

## Known limitations

- The library is linked at build time, so `{PREFIX}_LIBRARY` isn't honored and
  library paths come from the linker and loader. A library the dynamic
  loader can't find stops the process before `main`, before `check()` can
  report anything.
- Calls made while the library doesn't match (whether or not `check()` ran),
  malformed value buffers, and failures of non-throwing calls stop the
  process instead of throwing.
- A type that shares its name with a module's namespace `enum` (the `Stats`
  record beside the `kv.stats` module's `Kv.Stats`) is qualified with the
  Swift module name where the two would clash (`Kvstore.Stats`).
- The runtime declares the public types `{Module}Library`,
  `{Module}RuntimeError`, and `NativeSequence`; an IDL type with one of
  those names doesn't compile.
- The manifest stays at `swift-tools-version:5.9` for Xcode 15 users, so
  SwiftPM compiles the package in the Swift 5 language mode even under a
  Swift 6 toolchain.
