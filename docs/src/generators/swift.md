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
| `StructName` | `StructName` (class)        | Wraps `OpaquePointer`            |
| `InterfaceName` | `InterfaceName` (`final class`) | Wraps `OpaquePointer`; see [Interfaces](#interfaces) |
| `EnumName` (plain) | `EnumName` (`enum`)   | Backed by `UInt32`               |
| `EnumName` (rich)  | `EnumName` (class)    | Wraps `OpaquePointer`, like a struct |
| `T?`         | `T?`                        | Optional pointer / sentinel      |
| `[T]`        | `[T]`                       | Pointer + length                 |
| `iter<T>`    | `[T]`                       | Drained eagerly via `_next`      |

## Example IDL → generated code

```yaml
version: "0.5.0"
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

Structs are wrapper classes around an `OpaquePointer`. The `deinit` calls
the C destructor; computed properties call the C getters:

```swift
public class Contact {
    let ptr: OpaquePointer
    init(ptr: OpaquePointer) { self.ptr = ptr }
    deinit { weaveffi_contacts_Contact_destroy(ptr) }

    public var name: String {
        let raw = weaveffi_contacts_Contact_get_name(ptr)
        guard let raw = raw else { return "" }
        defer { weaveffi_free_string(raw) }
        return String(cString: raw)
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
        var err = weaveffi_error(code: 0, message: nil)
        let result: OpaquePointer? = name.withCString { name_ptr in
                return weaveffi_contacts_create_contact(name_ptr, age, &err)
        }
        try checkContacts(&err)
        guard let result = result else { throw WeaveFFIError.error(code: -1, message: "null pointer") }
        return Contact(ptr: result)
    }
}
```

Call it as `try Contacts.createContact(name: "Grace", age: 46)`. A
function without `throws` keeps a plain non-throwing signature; its only
possible failures are producer bugs, which trap via `fatalError`. Nested
IDL modules become nested namespace enums (`Kv.Stats.getStats(store:)` in
the `kvstore` sample).

Optionals and lists use `withOptionalPointer`, `withOptionalCString`,
and `withUnsafeBufferPointer` helpers:

```swift
@inline(__always)
func withOptionalPointer<T, R>(to value: T?, _ body: (UnsafePointer<T>?) throws -> R) rethrows -> R {
    guard let value = value else { return try body(nil) }
    return try withUnsafePointer(to: value) { try body($0) }
}

ids.withUnsafeBufferPointer { buf in
    let ids_ptr = buf.baseAddress
    let ids_len = buf.count
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
        var err = weaveffi_error(code: 0, message: nil)
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
`UInt32`; a rich enum instead becomes a wrapper `class` around an
`OpaquePointer` (same ownership model as a struct class) with a nested `Tag`,
throwing static factories, and per-variant computed properties. From the
`shapes` sample:

```swift
public class Shape {
    let ptr: OpaquePointer
    deinit { weaveffi_shapes_Shape_destroy(ptr) }

    public enum Tag: Int32 {
        case empty = 0
        case circle = 1
        case rectangle = 2
        case labeled = 3
    }
    public var tag: Tag { Tag(rawValue: weaveffi_shapes_Shape_tag(ptr))! }

    public static func empty() throws -> Shape
    public static func circle(radius: Double) throws -> Shape
    public static func rectangle(width: Float, height: Float) throws -> Shape
    public static func labeled(label: String, count: UInt8) throws -> Shape

    public var circleRadius: Double { get }
    public var rectangleWidth: Float { get }
    public var rectangleHeight: Float { get }
    public var labeledLabel: String { get }
    public var labeledCount: UInt8 { get }
}
```

Build a variant with its throwing factory, switch on `tag`, and read only the
matching property. Module functions live on the `Shapes` namespace enum and
take/return the wrapper:

```swift
let shape = try Shape.circle(radius: 2.0)

if shape.tag == .circle {
    print("radius = \(shape.circleRadius)")
}

print(Shapes.describe(shape: shape))
let bigger = Shapes.scale(shape: shape, factor: 3.0)
```

Ownership matches struct classes: the `Shape` `deinit` calls
`weaveffi_shapes_Shape_destroy`, so ARC frees the handle when the last
reference goes away, no manual free required.

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

- Struct and interface classes own an `OpaquePointer`. The class `deinit`
  calls the matching C destructor.
- Returned strings are copied into Swift `String` and the raw pointer is
  freed via `weaveffi_free_string` immediately.
- `withUnsafeBufferPointer` and `withOptionalPointer` keep input buffers
  alive only for the duration of the C call; there's no copy.
- For `bytes` parameters, the wrapper copies the `Data` into a
  `[UInt8]` array and passes it via `withUnsafeBufferPointer`; returned
  `bytes` are copied into `Data` and the Rust buffer is freed with
  `weaveffi_free_bytes`.

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
            weaveffi_tasks_run_task_async(name_ptr, { context, err, result in
                let contRef = Unmanaged<ContinuationRef<TaskResult, Error>>.fromOpaque(context!).takeRetainedValue()
                if let err = err, err.pointee.code != 0 {
                    let code = err.pointee.code
                    let msg = err.pointee.message.flatMap { String(cString: $0) } ?? ""
                    contRef.value.resume(throwing: mapTasks(code: code, message: msg))
                } else {
                    guard let result = result else {
                        contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: "null pointer"))
                        return
                    }
                    contRef.value.resume(returning: TaskResult(ptr: result))
                }
            }, ctx)
        }
    }
}
```

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

`iter<T>` returns are drained eagerly: the wrapper calls the generated
`_next` C function until it reports exhaustion, frees each element,
destroys the iterator handle, and returns a Swift array. From the
`events` sample (`get_messages` returns `iter<string>` and doesn't
declare `throws`, so the wrapper is non-throwing and traps on producer
bugs):

```swift
public static func getMessages() -> [String] {
    var err = weaveffi_error(code: 0, message: nil)
    let iter = weaveffi_events_get_messages(&err)
    trap(&err)
    guard let iter = iter else { return [] }
    var items: [String] = []
    var iterItem: UnsafePointer<CChar>? = nil
    var iterErr = weaveffi_error(code: 0, message: nil)
    while weaveffi_events_GetMessagesIterator_next(iter, &iterItem, &iterErr) != 0 {
        items.append(String(cString: iterItem!))
        weaveffi_free_string(UnsafeMutablePointer(mutating: iterItem))
    }
    weaveffi_events_GetMessagesIterator_destroy(iter)
    trap(&iterErr)
    return items
}
```

A throwing iterator function (like `Store.listKeys(prefix:)` in the
`kvstore` sample) uses the typed checker instead and declares `throws`.

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
- **Optional struct ends up `nil` even when present**: the C function
  is allowed to return a null pointer to indicate absence; double-check
  the Rust implementation actually returns `Some(_)` for the case you
  expect.
