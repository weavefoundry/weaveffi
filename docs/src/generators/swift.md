# Swift

## Overview

The Swift target emits a SwiftPM System Library (`CWeaveFFI`) that
references the generated C header via a `module.modulemap`, plus a thin
Swift module (`WeaveFFI`) that wraps the C ABI in idiomatic Swift with
`throws`-based error handling and Swift-native types.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/swift/Package.swift` | SwiftPM manifest declaring `CWeaveFFI` (system library) and `WeaveFFI` (Swift wrapper) |
| `generated/swift/Sources/CWeaveFFI/module.modulemap` | C module map pointing at the generated header |
| `generated/swift/Sources/WeaveFFI/WeaveFFI.swift` | Swift wrapper: enums, struct classes, namespaced module functions |

The module name shown above (`WeaveFFI`) is the default. It is overridden by
`[swift] module_name` or, failing that, by the IDL
[`package:` name](../reference/idl.md#package-metadata) PascalCased
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
| `bytes`      | `Data` / `[UInt8]`          | Pointer + length                 |
| `handle`     | `UInt64`                    | Direct value                     |
| `StructName` | `StructName` (`struct`)     | Plain value type; crosses as a value buffer |
| `InterfaceName` | `InterfaceName` (`final class`) | Wraps `OpaquePointer`; see [Interfaces](#interfaces) |
| `EnumName` (plain) | `EnumName` (`enum`)   | Backed by `UInt32`               |
| `EnumName` (rich)  | `EnumName` (`enum` with associated values) | Crosses as a value buffer |
| `T?`         | `T?`                        | Value buffer; `Interface?` stays a nullable pointer |
| `[T]`        | `[T]`                       | Value buffer                     |
| `{K: V}`     | `[K: V]`                    | Value buffer                     |
| `iter<T>`    | generated `Sequence` class  | Lazy; one `_next` call per step  |

## Example IDL → generated code

```yaml
version: "0.7.0"
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
default (`strip_module_prefix = false` in `[swift]` restores it), since the
namespace enum already scopes the name. A function with `throws: true`
becomes a Swift `throws` function delivering the typed domain error. String
parameters are passed as NUL-terminated C strings via `withCString`:

```swift
public enum Contacts {
    public static func createContact(name: String, age: Int32) throws -> Contact {
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        var outLen: Int = 0
        let result = name.withCString { name_ptr in
                return weaveffi_contacts_create_contact(name_ptr, age, &outLen, &err)
        }
        try checkContacts(&err)
        // Decode the returned value buffer, then release it.
        var reader = WvReader(ptr: result, len: outLen)
        let value = wvReadContact(&reader)
        weaveffi_free_bytes(result, outLen)
        return value
    }
}
```

Call it as `try Contacts.createContact(name: "Grace", age: 46)`. A
function without `throws` keeps a plain non-throwing signature; its only
possible failures are producer bugs, which trap via `fatalError`. Nested
IDL modules become nested namespace enums (`Kv.Stats.getStats(store:)` in
the `kvstore` sample).

Optionals, lists, and maps are buffered: the wrapper packs the argument
into a `WvWriter` and passes the resulting bytes as a borrowed
pointer-plus-length pair for the duration of the call:

```swift
var w = WvWriter()
// ... generated write statements for the optional/list/map argument ...
let result = w.bytes.withUnsafeBufferPointer { buf in
    weaveffi_contacts_find_contact(buf.baseAddress, buf.count, &err)
}
```

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
doesn't declare (a producer panic, for example):

```swift
do {
    let entry = try store.get(key: "alpha")
} catch KvError.keyNotFound {
    print("no such key")
} catch let e as KvError {
    print("kv failure \(e.errorCode)")
}
```

Callables without `throws` have plain, non-throwing signatures. They still
check the error slot after the call, but a non-zero code there can only be
a producer bug, so it traps with `fatalError("\(code): \(message)")`
instead of throwing.

An error code that declares payload `fields:` carries them as additional
labeled associated values on its case, alongside `message:`, decoded from
the error's payload buffer before `weaveffi_error_clear` releases it.

## Interfaces

An `interfaces:` entry becomes a `final class` owning an `OpaquePointer`,
with `deinit` calling the implicit C destructor. A constructor named `new`
becomes `init`; any other constructor becomes a throwing static factory.
Methods are instance methods and statics are static methods, all in
lowerCamelCase with argument labels. From the `kvstore` sample's `Store`
(trimmed):

```swift
/// An embedded key-value store owning its entries
public final class Store {
    let ptr: OpaquePointer

    deinit {
        weaveffi_kv_Store_destroy(ptr)
    }

    /// Open (or create) a store backed by the given filesystem path
    public static func open(path: String) throws -> Store {
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        let result: OpaquePointer? = path.withCString { path_ptr in
                return weaveffi_kv_Store_open(path_ptr, &err)
        }
        try checkKv(&err)
        guard let result = result else { throw WeaveFFIError.error(code: -1, message: "null pointer") }
        return Store(ptr: result)
    }

    /// Remove the entry for the given key, returning true if it existed
    public func delete(key: String) throws -> Bool

    /// Return the number of live entries in the store
    public func count() -> Int64          // no throws: traps on producer bugs

    /// Stream every key, optionally filtered by a prefix
    public func listKeys(prefix: String?) throws -> KvStoreListKeysIterator

    /// Reclaim space asynchronously; returns the number of bytes reclaimed
    public func compact() async throws -> Int64

    /// Legacy single-shot put kept for compatibility
    @available(*, deprecated, message: "use put() with explicit kind")
    public func legacyPut(key: String, value: Data) throws -> Bool

    /// The largest number of live entries one store will hold
    public static func defaultCapacity() -> Int64
}
```

```swift
let store = try Store.open(path: "/tmp/cache.kv")
_ = try store.put(key: "alpha", value: Data("1".utf8), kind: .volatile, ttlSeconds: nil)
print(store.count())
let reclaimed = try await store.compact()
```

The `contacts` sample's `ContactBook` declares a constructor named `new`,
which surfaces as a real initializer: `let book = ContactBook()`. ARC
releases the underlying object when the last reference goes away; there's
no manual `close`. An interface parameter is borrowed for the call (the
wrapper passes its pointer); an interface return wraps the owned pointer
in a new instance.

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
codec pair. Values are plain Swift data; nothing to free.

## Build instructions

Build the producer cdylib, generate the bindings, and compile your
program against the generated Swift module (the `contacts` sample shown;
its module resolves to `Contacts` + `CContacts` from the package name):

```bash
cargo build -p contacts
weaveffi generate samples/contacts/contacts.yml -o generated

swiftc \
  -I generated/swift/Sources/CContacts \
  -L target/debug -lcontacts \
  -Xlinker -rpath -Xlinker target/debug \
  generated/swift/Sources/Contacts/Contacts.swift main.swift -o app

DYLD_LIBRARY_PATH=target/debug ./app   # LD_LIBRARY_PATH on Linux
```

In a real SwiftPM application, add the generated package as a path
dependency, link the system-library and wrapper targets, and ship the
cdylib as part of an XCFramework or bundled `.dylib`/`.so`. The
`conformance/swift/` consumers show a complete SwiftPM assembly for every
sample (see `conformance/run.sh`).

## Memory and ownership

- Interface classes own an `OpaquePointer`. The class `deinit` calls
  the matching C destructor. Structs and rich enums are plain Swift
  values with nothing to free.
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
  decoded with the matching `wvRead*` routine and the producer's buffer
  is released with `weaveffi_free_bytes`.

## Async support

Async IDL functions (`async: true`) are exposed as `async throws`
methods that bridge the C ABI completion callback into Swift structured
concurrency via `withCheckedThrowingContinuation`. The continuation is
boxed in a `ContinuationRef`, retained with `Unmanaged.passRetained`,
and released exactly once, by `takeRetainedValue()` inside the C
completion callback. From the `async-demo` sample:

```swift
private final class ContinuationRef<T, E: Error> {
    let value: CheckedContinuation<T, E>
    init(_ value: CheckedContinuation<T, E>) { self.value = value }
}

public static func runTask(name: String) async throws -> TaskResult {
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<TaskResult, Error>) in
        let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()
        name.withCString { name_ptr in
            weaveffi_tasks_run_task_async(name_ptr, { context, err, resultPtr, resultLen in
                let contRef = Unmanaged<ContinuationRef<TaskResult, Error>>.fromOpaque(context!).takeRetainedValue()
                if let err = err, err.pointee.code != 0 {
                    let code = err.pointee.code
                    let msg = err.pointee.message.flatMap { String(cString: $0) } ?? ""
                    weaveffi_error_free(err)
                    contRef.value.resume(throwing: mapTasks(code: code, message: msg))
                } else {
                    // TaskResult is a record: copy the owned value buffer,
                    // free it, then decode from the copy.
                    let resultBytes = [UInt8](UnsafeBufferPointer(start: resultPtr, count: resultLen))
                    weaveffi_free_bytes(UnsafeMutablePointer(mutating: resultPtr), resultLen)
                    var reader = WvReader(bytes: resultBytes)
                    contRef.value.resume(returning: wvReadTaskResult(&reader))
                }
            }, ctx)
        }
    }
}
```

The completion callback fires exactly once, on an arbitrary producer
thread, and the continuation is resumed exactly once from inside it.
Result buffers passed to the callback (strings, bytes, and buffered
values) are owned by the consumer: the wrapper copies or decodes them
(for example `String(cString:)`, or the byte-array copy above) and
then releases them with `weaveffi_free_string` or
`weaveffi_free_bytes`. A reported error is heap-boxed: the wrapper
copies its code, message, and payload, then releases it with
`weaveffi_error_free`. An owned interface result transfers ownership
too: the callback adopts the pointer into a new wrapper instance,
whose `deinit` eventually frees it.

`run_task` declares `throws: true`, so the continuation rejects with the
typed `TaskError` (via `mapTasks`). An async callable without `throws` is
`async` but not `throws`: it uses a plain `withCheckedContinuation` whose
failure type is `Never`, and a producer bug traps instead.

For callables marked `cancellable: true`, the C ABI takes an extra
`weaveffi_cancel_token*` parameter. The Swift wrapper passes `nil` for
that slot; cancellation isn't surfaced in Swift, and Swift `Task`
cancellation doesn't propagate to the native operation (from the
`kvstore` sample's `Store.compact`):

```swift
weaveffi_kv_Store_compact_async(ptr, nil, { context, err, result in
```

## Callbacks and listeners

IDL `callbacks` paired with `listeners` produce a register/unregister
pair. From the `events` sample:

```yaml
modules:
  - name: events
    callbacks:
      - name: OnMessage
        params:
          - { name: message, type: string }
    listeners:
      - name: message_listener
        event_callback: OnMessage
```

Registration is a static method on the module's namespace enum: it
takes a plain Swift closure and returns a `UInt64` subscription id;
pass that id back to unregister. The closure is boxed
(`WvCallbackBox`), retained with `Unmanaged.passRetained`, and handed
to the C ABI as the `void* context` of a C trampoline. The context
pointer is kept in a global `wvListenerContexts` dictionary keyed by
subscription id and guarded by an `NSLock` (`wvListenerLock`);
unregistering removes the entry and releases the box:

```swift
public static func registerMessageListener(_ callback: @escaping (String) -> Void) -> UInt64 {
    let box = WvCallbackBox(callback)
    let ctx = Unmanaged.passRetained(box).toOpaque()
    let id = weaveffi_events_register_message_listener({ message, context in
        let cb = Unmanaged<WvCallbackBox<(String) -> Void>>.fromOpaque(context!).takeUnretainedValue().value
        cb(String(cString: message!))
    }, ctx)
    wvListenerLock.lock()
    wvListenerContexts[id] = ctx
    wvListenerLock.unlock()
    return id
}

public static func unregisterMessageListener(_ id: UInt64) {
    weaveffi_events_unregister_message_listener(id)
    wvListenerLock.lock()
    let ctx = wvListenerContexts.removeValue(forKey: id)
    wvListenerLock.unlock()
    if let ctx = ctx {
        Unmanaged<WvCallbackBox<(String) -> Void>>.fromOpaque(ctx).release()
    }
}
```

The callback runs on the producer's thread, whichever thread the
native side fires the event from. For UI work, hop to the main thread
yourself (e.g. `DispatchQueue.main.async` or `await MainActor.run`).

## Iterators

`iter<T>` returns are lazy: the wrapper returns a generated
`final class` conforming to `Sequence` and `IteratorProtocol` that
wraps the opaque C iterator handle. Nothing is drained up front; each
`next()` call pulls exactly one element from the producer, copies it
into Swift memory, and frees the element's native allocation (strings
via `weaveffi_free_string`). From the `events` sample (`get_messages`
returns `iter<string>` and doesn't declare `throws`, so the wrapper is
non-throwing and traps on producer bugs):

```swift
/// A lazy sequence over the `String` elements streamed by `weaveffi_events_get_messages`.
///
/// Each `next()` call pulls exactly one element from the producer. The
/// underlying C iterator is destroyed eagerly on exhaustion and from
/// `deinit` when iteration is abandoned early.
public final class EventsGetMessagesIterator: Sequence, IteratorProtocol {
    private var handle: OpaquePointer?

    deinit {
        destroyHandle()
    }

    /// Pulls the next element from the producer, or returns `nil` once the
    /// stream is exhausted (destroying the underlying iterator).
    public func next() -> String? {
        guard let handle = handle else { return nil }
        var item: UnsafePointer<CChar>? = nil
        var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
        if weaveffi_events_GetMessagesIterator_next(handle, &item, &err) == 0 {
            // ... a non-zero code is a producer bug: fatalError ...
            destroyHandle()
            return nil
        }
        let element = String(cString: item!)
        weaveffi_free_string(item)
        return element
    }
}

public static func getMessages() -> EventsGetMessagesIterator {
    var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)
    let iter = weaveffi_events_get_messages(&err)
    trap(&err)
    guard let iter = iter else { fatalError("-1: null iterator") }
    return EventsGetMessagesIterator(handle: iter)
}
```

The handle is destroyed exactly once: eagerly when `next()` reports
exhaustion, or from `deinit` when the sequence is abandoned early
(`destroyHandle` nulls the stored handle, so a double destroy is
impossible). Since the class conforms to `Sequence`, callers just
write `for message in Events.getMessages() { ... }`; the sequence is
single-pass.

A throwing iterator function like the `kvstore` sample's
`Store.listKeys(prefix:)` declares `throws` and routes a launch
failure through the typed checker (`checkKv`). Swift's
`IteratorProtocol.next()` can't throw, so a mid-stream error can't
surface on the step that failed; instead it ends iteration and is
stored in the sequence's public `error` property for the caller to
inspect after the loop:

```swift
/// If the producer reports an error mid-stream, iteration ends and the
/// error is stored in ``error`` for the caller to inspect after the loop.
public final class KvStoreListKeysIterator: Sequence, IteratorProtocol {
    /// The error that ended iteration early, if any.
    public private(set) var error: Error?
    // ... same handle lifecycle as above; a failing next() maps the
    // code through mapKv, stores it in `error`, and returns nil ...
}

let keys = try store.listKeys(prefix: nil)
for key in keys { print(key) }
if let error = keys.error { throw error }
```

## Troubleshooting

- **`module 'CWeaveFFI' not found`**: Xcode/SwiftPM didn't pick up
  the generated `module.modulemap`. Make sure
  `Sources/CWeaveFFI/module.modulemap` is on disk and the package
  declares `systemLibrary(name: "CWeaveFFI")`.
- **`Library not loaded: libweaveffi.dylib`**: set
  `DYLD_LIBRARY_PATH` for development or embed the dylib in your
  application bundle for distribution.
- **Crashes after `deinit`**: never reuse an `OpaquePointer` after the
  owning Swift wrapper goes out of scope. The C side has already freed
  it.
- **Optional value ends up `nil` even when present**: an optional
  crosses inside a value buffer as a presence flag followed by the
  value; double-check the Rust implementation actually returns
  `Some(_)` for the case you expect.
