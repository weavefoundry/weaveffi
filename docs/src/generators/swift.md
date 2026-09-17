# Swift

## Overview

The Swift target emits a SwiftPM System Library (`CWeaveFFI`) that
references the generated C header via a `module.modulemap`, plus a thin
Swift module (`WeaveFFI`) that wraps the C ABI in idiomatic Swift:
`throws`-based error handling, plain structs and enums for values,
ARC-managed `final class` wrappers for reference-counted objects, Swift
protocols for callback interfaces, `async` methods for async callables,
and lazy `Sequence`s for iterators. The surface follows ABI revision 2.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/swift/Package.swift` | SwiftPM manifest declaring `CWeaveFFI` (system library) and `WeaveFFI` (Swift wrapper) |
| `generated/swift/Sources/CWeaveFFI/module.modulemap` | C module map pointing at the generated header |
| `generated/swift/Sources/WeaveFFI/WeaveFFI.swift` | Swift wrapper: enums, structs, object classes, callback protocols, namespaced module functions |

The module name shown above (`WeaveFFI`) is the default. It is overridden by
`[generators.swift] module_name` in `weaveffi.toml` or, failing that, by the
[`[package]` name](../guides/config.md#package) PascalCased
(`async-demo` → `AsyncDemo`). The Swift wrapper, its `Sources/<Module>/`
directory, the system-library target, and its `Sources/C<Module>/` module map
all move together (e.g. `AsyncDemo` + `CAsyncDemo`), so the generated package
stays buildable under any name.

## Type mapping

| IDL type     | Swift type                  | Notes                            |
|--------------|-----------------------------|----------------------------------|
| `i32`        | `Int32`                     | Direct value                     |
| `u32`        | `UInt32`                    | Direct value                     |
| `i64`        | `Int64`                     | Direct value                     |
| `u64`        | `UInt64`                    | Direct value                     |
| `i8`         | `Int8`                      | Direct value                     |
| `i16`        | `Int16`                     | Direct value                     |
| `u8`         | `UInt8`                     | Direct value                     |
| `u16`        | `UInt16`                    | Direct value                     |
| `f32`        | `Float`                     | Direct value                     |
| `f64`        | `Double`                    | Direct value                     |
| `bool`       | `Bool`                      | C `bool` at the ABI              |
| `string`     | `String`                    | NUL-terminated UTF-8 (`withCString`) |
| `bytes`      | `Data`                      | Pointer + length                 |
| `StructName` | `StructName` (`struct`)     | Plain value type; crosses as a value buffer |
| `InterfaceName` | `InterfaceName` (`final class`) | One strong reference per instance; see [Objects](#objects-interfaces) |
| `InterfaceName?` | `InterfaceName?`         | Nullable pointer at the ABI      |
| `CallbackName` | `protocol CallbackName`   | Implement and pass any conforming value; see [Callback interfaces](#callback-interfaces) |
| `EnumName` (plain) | `EnumName` (`enum`)   | Backed by `UInt32`               |
| `EnumName` (rich)  | `EnumName` (`enum` with associated values) | Crosses as a value buffer |
| `T?`         | `T?`                        | Value buffer                     |
| `[T]`        | `[T]`                       | Value buffer                     |
| `{K: V}`     | `[K: V]`                    | Value buffer                     |
| `iter<T>`    | generated `Sequence` class  | Lazy; one `_next` call per step  |

64-bit integers are native `Int64`/`UInt64`. `Float` and `Double` cross the
ABI as IEEE values (the value-buffer codec writes `bitPattern`), so NaN,
the infinities, and `-0.0` round-trip unchanged; the `codec` sample's
`Codec.roundtripU64` and `Codec.roundtripF64` exercise this.

## Example IDL → generated code

```yaml
version: "0.9.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }

    errors:
      name: ContactsError
      codes:
        - { name: InvalidName, code: 1, message: "name must not be empty" }
        - { name: NotFound, code: 2, message: "contact not found" }

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: age, type: i32 }
        return: Contact
        throws: true

      - name: find_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"
        throws: true

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: set_type
        params:
          - { name: id, type: i32 }
          - { name: contact_type, type: ContactType }
```

Enums become Swift enums with lowerCamelCase cases backed by `UInt32`:

```swift
public enum ContactType: UInt32 {
    case personal = 0
    case work = 1
    case other = 2
}
```

Structs are plain Swift structs with typed properties and a public
memberwise initializer. They declare no C symbols; a `Contact` crosses
the ABI serialized in the [value-buffer format](../reference/value-buffers.md)
as a single pointer-plus-length pair, written and read by the generated
`wvWriteContact`/`wvReadContact` codec pair over the private
`WvWriter`/`WvReader` helpers:

```swift
public struct Contact {
    public var name: String
    public var email: String?
    public var age: Int32

    public init(name: String, email: String?, age: Int32) {
        self.name = name
        self.email = email
        self.age = age
    }
}
```

Module functions live as static methods on a namespace enum in
lowerCamelCase with real argument labels; the module prefix is stripped by
default (`strip_module_prefix = false` in `[generators.swift]` restores it), since the
namespace enum already scopes the name. A function with `throws: true`
becomes a Swift `throws` function delivering the typed domain error. String
parameters are passed as NUL-terminated C strings via `withCString`:

```swift
public enum Contacts {
    public static func createContact(name: String, age: Int32) throws -> Contact {
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        var outLen: Int = 0
        let rv = name.withCString { name_ptr in
            return weaveffi_contacts_create_contact(name_ptr, age, &outLen, &err)
        }
        try checkContacts(&err)
        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: "null buffer") }
        let rvBytes = [UInt8](UnsafeBufferPointer(start: rv, count: outLen))
        weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)
        var rvReader = WvReader(bytes: rvBytes)
        let v0 = wvReadContact(&rvReader)
        rvReader.finish()
        return v0
    }
}
```

Call it as `try Contacts.createContact(name: "Grace", age: 46)`. A
function without `throws` keeps a plain non-throwing signature; a failure
reported there traps via `fatalError` (see [Runtime error
codes](#runtime-error-codes)). Nested IDL modules become nested namespace
enums (`Kv.Stats.getStats(store:)` in the `kvstore` sample).

Optionals, lists, and maps are buffered: the wrapper packs the argument
into a `WvWriter` and passes the resulting bytes as a borrowed
pointer-plus-length pair for the duration of the call.

## Objects (interfaces)

An `interfaces:` entry becomes a `final class` holding an `OpaquePointer`
that is one strong reference to a reference-counted producer object. A
constructor named `new` becomes `init`; any other constructor becomes a
static factory. Methods are instance methods and statics are static
methods, all in lowerCamelCase with argument labels. From the `kvstore`
sample's `Store`:

```swift
public final class Store {
    let ptr: OpaquePointer

    init(ptr: OpaquePointer) {
        self.ptr = ptr
    }

    deinit {
        weaveffi_kv_Store_destroy(ptr)
    }

    /// Returns a new strong reference to the same object, for a position that
    /// takes ownership of it (an object token inside a value buffer).
    func clonePtr() -> OpaquePointer {
        weaveffi_kv_Store_clone(ptr)!
    }

    public static func open(path: String) throws -> Store {
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        let rv: OpaquePointer? = path.withCString { path_ptr in
            return weaveffi_kv_Store_open(path_ptr, &err)
        }
        try checkKv(&err)
        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: "null pointer") }
        return Store(ptr: rv)
    }

    public func delete(key: String) throws -> Bool
    public func count() -> Int64
    public func listKeys(prefix: String?) throws -> KvStoreListKeysIterator
    public func compact() async throws -> Int64
    public static func defaultCapacity() -> Int64
}
```

```swift
let store = try Store.open(path: "/tmp/cache.kv")
_ = try store.put(key: "alpha", value: Data("1".utf8), kind: .volatile, ttlSeconds: nil)
print(store.count())
let reclaimed = try await store.compact()
```

- **Disposal is ARC.** Each wrapper instance owns exactly one strong
  reference and `deinit` releases it with `_destroy`. There is no public
  `close()`; drop your last Swift reference and the producer drops the
  object when its own last reference goes. Because the wrapper is a class,
  assigning it around shares one Swift object (and one native reference);
  you never see a use-after-release from Swift code alone.
- **Copies mint new references at the boundary.** The internal
  `clonePtr()` calls `_clone` whenever the wrapper must hand the producer a
  reference it will own (an object inside a value buffer); a returned
  object is adopted into a new instance. `share()` in the sample returns a
  second wrapper over the same producer object, and the two are
  independently released.
- The `events` sample's `EventBus` declares a constructor named `new`,
  which surfaces as a real initializer: `let bus = EventBus()`.

Deprecated members carry `@available(*, deprecated, message:)`.

### Objects as parameters, returns, and inside values

A top-level object parameter is borrowed for the call (the wrapper passes
`ptr`); a returned object is adopted. `Store?` is a nullable pointer both
ways:

```swift
public func larger(other: Store?) -> Store? {
    var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
    let rv = weaveffi_kv_Store_larger(ptr, other?.ptr, &err)
    trap(&err)
    return rv.map { Store(ptr: $0) }
}
```

An object inside a record, list, map value, optional, or rich-enum payload
is an ordinary property (`public var store: Store; public var mirror: Store?`
in `StoreInfo`). On the wire it's a `u64` token: the codec writes a fresh
`clonePtr()` for each object (so your wrapper keeps its own reference) and
adopts the token on read:

```swift
func wvWriteStoreInfo(_ value: StoreInfo, into w: inout WvWriter) {
    w.writeString(value.label)
    w.writeObject(value.store.clonePtr())
    if let v0 = value.mirror {
        w.writeOptionFlag(true)
        w.writeObject(v0.clonePtr())
    } else {
        w.writeOptionFlag(false)
    }
    w.writeInt64(value.count)
}

func wvReadStoreInfo(_ r: inout WvReader) -> StoreInfo {
    let v1 = r.readString()
    let v2 = Store(ptr: r.readObject())
    // ...
}
```

`Store.openMany(paths:)` returns `[Store]`, one adopted instance per
element, and `Store.totalCount(stores:extra:)` clones each object it
encodes. An async callable returning an object adopts the pointer inside
the completion callback, and an `iter<Interface>` adopts one per `next()`.

## Typed errors

A module's error domain becomes a Swift error enum conforming to `Error`
and `LocalizedError`, with one lowerCamelCase case per declared code, each
carrying its message. From the `kvstore` sample's `KvError`:

```swift
/// Typed errors reported by the `kv` module.
public enum KvError: Error, LocalizedError {
    case keyNotFound(message: String)
    case expired(message: String)
    case storeFull(message: String)
    case ioError(message: String)

    /// The numeric ABI code carried by this error.
    public var errorCode: Int32 {
        switch self {
        case .keyNotFound: return 1001
        case .expired: return 1002
        case .storeFull: return 1003
        case .ioError: return 1004
        }
    }
}
```

Callables with `throws: true` route failures through a per-domain checker
(`checkKv`) that maps the ABI code to the matching case, falling back to
the generic `WeaveFFIError.error(code:message:)` for codes the domain
doesn't declare:

```swift
do {
    let entry = try store.get(key: "alpha")
} catch KvError.keyNotFound {
    print("no such key")
} catch let e as KvError {
    print("kv failure \(e.errorCode)")
} catch let e as WeaveFFIError {
    print("runtime failure \(e.errorCode): \(e.localizedDescription)")
}
```

An error code that declares payload `fields:` carries them as additional
labeled associated values on its case, alongside `message:`, decoded from
the error's payload buffer before `weaveffi_error_clear` releases it.

### Runtime error codes

Negative codes are never domain codes, so they always arrive as
`WeaveFFIError.error(code:message:)`:

| Code | ABI name | When |
|------|----------|------|
| -1 | `GENERIC_ERROR_CODE` | The producer reported a failure with no domain code |
| -2 | `PANIC_ERROR_CODE` | The Rust producer panicked inside an export or a spawned async future |
| -3 | `MARSHAL_ERROR_CODE` | A null object or a malformed value buffer or string was rejected at the boundary |
| -4 | `FOREIGN_ERROR_CODE` | A callback-interface implementation threw |

Where they surface depends on the callable:

- A callable with `throws: true` throws `WeaveFFIError` (sync), rejects its
  continuation (async), or, for an iterator, ends iteration and stores the
  error in the sequence's `error` property.
- A callable without `throws` has a non-throwing Swift signature. The
  wrapper still checks the error slot after the call and **traps** with
  `fatalError("\(code): \(message)")` when it's nonzero; an async one traps
  inside the completion callback. This is the trap idiom: -1 through -3
  there are producer bugs, and -4 means a callback you passed threw on a
  path the IDL declared as infallible.
- A malformed value buffer detected on the Swift side (`wvDecodeFailure`)
  also traps.

## Callback interfaces

A `callback_interfaces:` entry becomes a Swift protocol with one `throws`
requirement per IDL method. Conform any class or struct to it and pass the
value wherever the API expects the interface. From the `events` sample:

```swift
public protocol Subscriber {
    /// Decide how the bus should treat `topic` for this subscriber.
    func route(topic: String) throws -> Delivery
    /// Receive an accepted message. Returns the subscriber's running count
    /// of received messages.
    func onMessage(message: Message) throws -> Int64
    /// Receive the bus itself (an object handed through a callback). The
    /// consumer adopts the reference and may keep or drop it.
    func onAttached(bus: EventBus) throws
}
```

```swift
final class LoggingSubscriber: Subscriber {
    var attachedBus: EventBus?

    func route(topic: String) throws -> Delivery {
        topic == "quiet" ? .skip : .accept
    }
    func onMessage(message: Message) throws -> Int64 {
        print("\(message.topic): \(message.text)")
        return 1
    }
    func onAttached(bus: EventBus) throws {
        attachedBus = bus   // keep it, or let it go to release the reference
    }
}

let bus = EventBus()
_ = bus.subscribe(subscriber: LoggingSubscriber())
```

The wrapper boxes your value (`WvSubscriberBox`), retains it with
`Unmanaged.passRetained`, and passes the box as the vtable `ctx` along with
a process-wide static vtable of `@convention(c)` trampolines. The vtable's
`free` entry releases the box when the producer drops its last reference,
which is when your object is deallocated if nothing else holds it:

```swift
enum WvSubscriberVtable {
    static let value = weaveffi_events_Subscriber_vtable(
        route: { ctx, topic, out_err in
            let wvBox = Unmanaged<WvSubscriberBox>.fromOpaque(ctx!).takeUnretainedValue()
            do {
                return weaveffi_events_Delivery(try wvBox.impl.route(topic: String(cString: topic!)).rawValue)
            } catch {
                wvForeignError(out_err, error)
                return weaveffi_events_Delivery(0)
            }
        },
        // on_message, on_attached ...
        free: { ctx in
            Unmanaged<WvSubscriberBox>.fromOpaque(ctx!).release()
        }
    )
}
```

- **Argument ownership.** Strings and buffered values are copied into
  Swift values before your method runs. An object argument (`bus:
  EventBus`) is adopted into a fresh wrapper that your implementation
  owns: store it to keep the reference, or let it fall out of scope and
  `deinit` releases it.
- **Lifetime.** The producer may retain the implementation indefinitely
  (`subscribe` holds it until `clearSubscribers()` or the bus is dropped);
  a function that only uses it for the call (`Events.routeOnce`) frees it
  before returning.
- **Errors.** A thrown Swift error never unwinds through the C frame. The
  trampoline reports it with `weaveffi_error_set(out_err, -4, message)`,
  using `errorDescription` for a `LocalizedError` and
  `String(describing:)` otherwise. The producer aborts the call that
  triggered the callback and the original caller sees
  `WeaveFFIError.error(code: -4, ...)` if the callable throws; on a
  non-throwing callable the wrapper traps. The implementation stays
  attached either way.
- **Threads.** Methods run on whichever thread the producer calls from: the
  calling thread for a synchronous call, a producer worker for an async one
  (`publishLater` in the sample). The wrapper doesn't hop to the main actor
  or any queue; synchronize shared state yourself.

## Rich (algebraic) enums

An enum whose variants declare `fields` is a *rich* (algebraic) enum, a sum
type with associated data. Plain C-style enums stay Swift `enum`s backed by
`UInt32`; a rich enum instead becomes a native Swift enum with labeled
associated values, one case per variant. From the `shapes` sample:

```swift
/// An algebraic shape (sum type with associated data)
public enum Shape {
    /// The empty shape
    case empty
    /// A circle with a radius
    case circle(radius: Double)
    /// An axis-aligned rectangle
    case rectangle(width: Float, height: Float)
    /// A labeled shape with a small count
    case labeled(label: String, count: UInt8)
}
```

Build variants directly and pattern-match as with any Swift enum. Module
functions live on the `Shapes` namespace enum and take/return the value:

```swift
let shape = Shape.circle(radius: 2.0)

if case let .circle(radius) = shape {
    print("radius = \(radius)")
}

print(Shapes.describe(shape: shape))
let bigger = Shapes.scale(shape: shape, factor: 3.0)
```

There are no C symbols behind a rich enum: on the wire it's a value
buffer holding the `i32` variant tag followed by the active variant's
fields, written and read by the generated `wvWriteShape`/`wvReadShape`
codec pair. Values are plain Swift data; nothing to free. A payload that
holds an object follows the token rules above.

## Build instructions

Build the producer cdylib, generate the bindings, and compile your
program against the generated Swift module (the `kvstore` sample shown;
its module resolves to `Kvstore` + `CKvstore` from the package name):

```bash
cargo build -p kvstore
weaveffi generate samples/kvstore/src/lib.rs -o generated --target c,swift

swiftc \
  -I generated/swift/Sources/CKvstore \
  -L target/debug -lkvstore \
  -Xlinker -rpath -Xlinker target/debug \
  generated/swift/Sources/Kvstore/Kvstore.swift main.swift -o app

DYLD_LIBRARY_PATH=target/debug ./app   # LD_LIBRARY_PATH on Linux
```

The module map points at `../../../c/<prefix>.h`, so generate the C target
alongside Swift. In a real SwiftPM application, add the generated package
as a path dependency, link the system-library and wrapper targets, and
ship the cdylib as part of an XCFramework or bundled `.dylib`/`.so`. The
`conformance/swift/` consumers show a complete assembly for every sample
(see `conformance/run.sh`).

## Packaging

`weaveffi package --target swift` emits a SwiftPM package whose C module is
a `binaryTarget` xcframework, with the prebuilt desktop libraries under
`swift/lib/<platform>/` and a README describing how to fuse them into
`C<Module>.xcframework` with `lipo` and `xcodebuild -create-xcframework`
(the one step that needs Apple tooling). Only desktop slices are bundled
(`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `windows-x64`);
iOS and Android slices aren't produced. See
[Packaging and Distribution](../guides/packaging.md).

## Memory and ownership

- Interface classes own one strong reference; `deinit` releases it.
  Structs and rich enums are plain Swift values with nothing to free.
- Returned strings are copied into Swift `String` and the raw pointer is
  freed via `weaveffi_free_string` immediately.
- `withUnsafeBufferPointer` keeps input buffers alive only for the
  duration of the C call; there's no copy.
- For `bytes` parameters, the wrapper copies the `Data` into a
  `[UInt8]` array and passes it via `withUnsafeBufferPointer`; returned
  `bytes` are copied into `Data` and the Rust buffer is freed with
  `weaveffi_free_bytes`.
- Buffered values (structs, rich enums, optionals, arrays, and
  dictionaries) cross as one value buffer: parameters are packed into a
  `WvWriter` whose bytes the producer borrows for the call; returns are
  copied out, released with `weaveffi_free_bytes`, and decoded with the
  matching `wvRead*` routine. Object fields inside are cloned on the way in
  and adopted on the way out.
- Callback-interface implementations are retained by the producer's box
  until its `free` runs.

## Async support

Async IDL functions (`async: true`) are exposed as `async` methods that
bridge the C ABI completion callback into Swift structured concurrency via
a checked continuation. The continuation is boxed in a `ContinuationRef`,
retained with `Unmanaged.passRetained`, and released exactly once by
`takeRetainedValue()` inside the completion callback. From the `kvstore`
sample:

```swift
public func compact() async throws -> Int64 {
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Int64, Error>) in
        let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()
        weaveffi_kv_Store_compact_async(ptr, nil, { context, err, result in
            let contRef = Unmanaged<ContinuationRef<Int64, Error>>.fromOpaque(context!).takeRetainedValue()
            if let err = err, err.pointee.code != 0 {
                let code = err.pointee.code
                let msg = err.pointee.message.flatMap { String(cString: $0) } ?? ""
                let payload: [UInt8]? = err.pointee.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.pointee.payload_len)) }
                weaveffi_error_free(err)
                contRef.value.resume(throwing: mapKv(code: code, message: msg, payload: payload))
            } else {
                contRef.value.resume(returning: result)
            }
        }, ctx)
    }
}
```

The completion callback fires exactly once, on an arbitrary producer
thread, and the continuation is resumed exactly once from inside it.
Result buffers passed to the callback are owned by the consumer: the
wrapper copies or decodes them and releases them with
`weaveffi_free_string` or `weaveffi_free_bytes`; a reported error is
heap-boxed and released with `weaveffi_error_free`; an object result is
adopted into a new wrapper whose `deinit` eventually releases it.

`compact` declares `throws: true`, so the continuation rejects with the
typed `KvError` (via `mapKv`) or the generic `WeaveFFIError` for a runtime
code. An async callable without `throws` (the `events` sample's
`publishLater`) is `async` but not `throws`: it uses a plain
`withCheckedContinuation` whose failure type is `Never`, and a nonzero
code in the callback traps with `fatalError`.

For callables marked `cancellable: true`, the C ABI takes an extra
`weaveffi_cancel_token*` parameter. The Swift wrapper passes `nil` for
that slot; cancellation isn't surfaced in Swift, and Swift `Task`
cancellation doesn't propagate to the native operation.

## Iterators

`iter<T>` returns are lazy: the wrapper returns a generated
`final class` conforming to `Sequence` and `IteratorProtocol` that
wraps the opaque C iterator handle. Nothing is drained up front; each
`next()` call pulls exactly one element from the producer, copies it
into Swift memory, and frees the element's native allocation. From the
`kvstore` sample (`Store.listKeys` returns `iter<string>`):

```swift
public final class KvStoreListKeysIterator: Sequence, IteratorProtocol {
    private var handle: OpaquePointer?
    /// The error that ended iteration early, if any.
    public private(set) var error: Error?

    deinit {
        destroyHandle()
    }

    private func destroyHandle() {
        guard let handle = handle else { return }
        weaveffi_kv_Store_ListKeysIterator_destroy(handle)
        self.handle = nil
    }

    public func next() -> String? {
        guard let handle = handle else { return nil }
        var item: UnsafePointer<CChar>? = nil
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        if weaveffi_kv_Store_ListKeysIterator_next(handle, &item, &err) == 0 {
            if err.code != 0 {
                // ... map the code through mapKv and store it in `error` ...
            }
            destroyHandle()
            return nil
        }
        let element = String(cString: item!)
        weaveffi_free_string(item)
        return element
    }
}
```

The handle is destroyed exactly once: eagerly when `next()` reports
exhaustion, or from `deinit` when the sequence is abandoned early. Since
the class conforms to `Sequence`, callers just write
`for key in try store.listKeys(prefix: nil) { ... }`; the sequence is
single-pass. A buffered element is decoded from its value buffer and
released with `weaveffi_free_bytes`; an object element is adopted into a
wrapper.

A throwing iterator function like `listKeys` routes a launch failure
through the typed checker (`checkKv`). Swift's `IteratorProtocol.next()`
can't throw, so a mid-stream error can't surface on the step that failed;
instead it ends iteration and is stored in the sequence's public `error`
property for the caller to inspect after the loop:

```swift
let keys = try store.listKeys(prefix: nil)
for key in keys { print(key) }
if let error = keys.error { throw error }
```

A non-throwing iterator function (the `events` sample's `messages()`)
traps on a mid-stream code instead.

## Known limitations

- Cancellation tokens aren't exposed; async wrappers always pass `nil`.
- Mid-stream iterator errors are stored in `error`, not thrown, and only
  for throwing callables.
- Non-throwing callables trap (`fatalError`) on any runtime code,
  including -4 from a callback you passed; declare `throws: true` on any
  callable whose callback implementations may fail.
- The module map references the C header by relative path
  (`../../../c/<prefix>.h`), so the C target must be generated next to the
  Swift one.
- Callback methods aren't marshalled to any particular thread or actor.

## Troubleshooting

- **`module 'CWeaveFFI' not found`**: Xcode/SwiftPM didn't pick up
  the generated `module.modulemap`. Make sure
  `Sources/CWeaveFFI/module.modulemap` is on disk and the package
  declares `systemLibrary(name: "CWeaveFFI")`.
- **`Library not loaded: libweaveffi.dylib`**: set
  `DYLD_LIBRARY_PATH` for development or embed the dylib in your
  application bundle for distribution.
- **`Fatal error: -4: ...`**: a callback implementation threw on a
  callable without `throws`. Either make the implementation not throw or
  mark the callable `throws: true` so the error is catchable.
- **Optional value ends up `nil` even when present**: an optional
  crosses inside a value buffer as a presence flag followed by the
  value; double-check the Rust implementation actually returns
  `Some(_)` for the case you expect.
