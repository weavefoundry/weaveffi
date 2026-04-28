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

## Type mapping

| IDL type     | Swift type                  | Notes                            |
|--------------|-----------------------------|----------------------------------|
| `i32`        | `Int32`                     | Direct value                     |
| `u32`        | `UInt32`                    | Direct value                     |
| `i64`        | `Int64`                     | Direct value                     |
| `f64`        | `Double`                    | Direct value                     |
| `bool`       | `Bool`                      | Mapped to `Int32` 0/1 at the ABI |
| `string`     | `String`                    | UTF-8 buffers + length           |
| `bytes`      | `Data` / `[UInt8]`          | Pointer + length                 |
| `handle`     | `UInt64`                    | Direct value                     |
| `StructName` | `StructName` (class)        | Wraps `OpaquePointer`            |
| `EnumName`   | `EnumName` (`enum`)         | Backed by `Int32`                |
| `T?`         | `T?`                        | Optional pointer / sentinel      |
| `[T]`        | `[T]`                       | Pointer + length                 |

## Example IDL → generated code

```yaml
version: "0.3.0"
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

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: age, type: i32 }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: set_type
        params:
          - { name: id, type: i32 }
          - { name: contact_type, type: ContactType }
```

Enums become Swift enums with lowerCamelCase cases backed by `Int32`:

```swift
public enum ContactType: Int32 {
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

Module functions live as static methods on a namespace enum and `try`
into Swift errors:

```swift
public enum Contacts {
    public static func create_contact(_ name: String, _ age: Int32) throws -> Contact {
        var err = weaveffi_error(code: 0, message: nil)
        let rv = weaveffi_contacts_create_contact(name_ptr, name_len, age, &err)
        try check(&err)
        guard let rv = rv else {
            throw WeaveFFIError.error(code: -1, message: "null pointer")
        }
        return Contact(ptr: rv)
    }
}
```

Optionals and lists use `withOptionalPointer` and
`withUnsafeBufferPointer` helpers:

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

## Build instructions

The runnable example uses the `calculator` sample.

macOS:

```bash
cargo build -p calculator

cd examples/swift
swiftc \
  -I ../../generated/swift/Sources/CWeaveFFI \
  -L ../../target/debug -lcalculator \
  -Xlinker -rpath -Xlinker ../../target/debug \
  Sources/App/main.swift -o .build/debug/App

DYLD_LIBRARY_PATH=../../target/debug .build/debug/App
```

Linux:

```bash
cargo build -p calculator

cd examples/swift
swiftc \
  -I ../../generated/swift/Sources/CWeaveFFI \
  -L ../../target/debug -lcalculator \
  -Xlinker -rpath -Xlinker ../../target/debug \
  Sources/App/main.swift -o .build/debug/App

LD_LIBRARY_PATH=../../target/debug .build/debug/App
```

In a real SwiftPM application, add the generated package as a path
dependency, link `CWeaveFFI` and `WeaveFFI`, and ship the cdylib as part
of an XCFramework or bundled `.dylib`/`.so`.

## Memory and ownership

- Struct classes own an `OpaquePointer`. The class `deinit` calls the
  matching C destructor.
- Returned strings are copied into Swift `String` and the raw pointer is
  freed via `weaveffi_free_string` immediately.
- `withUnsafeBufferPointer` and `withOptionalPointer` keep input buffers
  alive only for the duration of the C call — there is no copy.
- For `bytes` parameters, the wrapper uses `withUnsafeBytes` so Swift
  retains ownership.

## Async support

Async IDL functions are exposed as `async throws` methods that bridge
the C ABI callback into Swift's structured concurrency via
`withCheckedThrowingContinuation`:

```swift
public static func fetch_contact(_ id: Int32) async throws -> Contact {
    return try await withCheckedThrowingContinuation { cont in
        weaveffi_contacts_fetch_contact_async(id, { ctx, err, result in
            let cont = Unmanaged<ContWrapper>.fromOpaque(ctx!)
                .takeRetainedValue().cont
            if let err = err, err.pointee.code != 0 {
                cont.resume(throwing: WeaveFFIError.from(err.pointee))
            } else if let result = result {
                cont.resume(returning: Contact(ptr: result))
            } else {
                cont.resume(throwing: WeaveFFIError.error(code: -1,
                    message: "null result"))
            }
        }, Unmanaged.passRetained(ContWrapper(cont: cont)).toOpaque())
    }
}
```

When the IDL marks the function `cancel: true`, the generated wrapper
exposes a Swift `Task` cancellation handler that forwards to the
underlying `weaveffi_cancel_token`.

## Troubleshooting

- **`module 'CWeaveFFI' not found`** — Xcode/SwiftPM did not pick up
  the generated `module.modulemap`. Make sure
  `Sources/CWeaveFFI/module.modulemap` is on disk and the package
  declares `systemLibrary(name: "CWeaveFFI")`.
- **`Library not loaded: libweaveffi.dylib`** — set
  `DYLD_LIBRARY_PATH` for development or embed the dylib in your
  application bundle for distribution.
- **Crashes after `deinit`** — never reuse an `OpaquePointer` after the
  owning Swift wrapper goes out of scope. The C side has already freed
  it.
- **Optional struct ends up `nil` even when present** — the C function
  is allowed to return a null pointer to indicate absence; double-check
  the Rust implementation actually returns `Some(_)` for the case you
  expect.
