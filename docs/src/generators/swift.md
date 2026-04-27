# Swift

The Swift generator emits a SwiftPM System Library (`CWeaveFFI`) that
references the generated C header via a `module.modulemap`, and a thin
Swift module (`WeaveFFI`) that wraps the C API with Swift types and
`throws`-based error handling.

## Generated artifacts

- `generated/swift/Package.swift`
- `generated/swift/Sources/CWeaveFFI/module.modulemap` — C module map pointing at the generated header
- `generated/swift/Sources/WeaveFFI/WeaveFFI.swift` — thin Swift wrapper

## Generated code examples

Given this IDL definition:

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

### Enums

Enums map to Swift enums backed by `Int32`. Variant names are converted to
lowerCamelCase:

```swift
public enum ContactType: Int32 {
    case personal = 0
    case work = 1
    case other = 2
}
```

### Structs (opaque wrapper classes)

Structs are wrapped as Swift classes holding an `OpaquePointer` to the
Rust-allocated data. The `deinit` calls the C ABI destroy function to free
memory. Field access is through computed properties that call the C ABI
getters:

```swift
public class Contact {
    let ptr: OpaquePointer

    init(ptr: OpaquePointer) {
        self.ptr = ptr
    }

    deinit {
        weaveffi_contacts_Contact_destroy(ptr)
    }

    public var name: String {
        let raw = weaveffi_contacts_Contact_get_name(ptr)
        guard let raw = raw else { return "" }
        defer { weaveffi_free_string(raw) }
        return String(cString: raw)
    }

    public var email: String {
        let raw = weaveffi_contacts_Contact_get_email(ptr)
        guard let raw = raw else { return "" }
        defer { weaveffi_free_string(raw) }
        return String(cString: raw)
    }

    public var age: Int32 {
        return weaveffi_contacts_Contact_get_age(ptr)
    }
}
```

### Optional handling

Optional types map to Swift optionals (`T?`). For value returns, the
generator dereferences via `pointee`. For string optionals, it guards
against nil and frees the string. For struct optionals, it wraps the
pointer:

```swift
// Optional value return: -> Int32?
return rv?.pointee

// Optional string return: -> String?
guard let rv = rv else { return nil }
defer { weaveffi_free_string(rv) }
return String(cString: rv)

// Optional struct return: -> Contact?
return rv.map { Contact(ptr: $0) }
```

Optional value parameters use `withOptionalPointer` to pass a nullable
pointer to the C ABI:

```swift
@inline(__always)
func withOptionalPointer<T, R>(to value: T?, _ body: (UnsafePointer<T>?) throws -> R) rethrows -> R {
    guard let value = value else { return try body(nil) }
    return try withUnsafePointer(to: value) { try body($0) }
}
```

### Array/List handling

List types map to Swift arrays (`[T]`). Parameters are passed using
`withUnsafeBufferPointer` to provide pointer+length to the C ABI. Return
values are converted from a C pointer+length pair:

```swift
// List parameter: [Int32]
ids.withUnsafeBufferPointer { ids_buf in
    let ids_ptr = ids_buf.baseAddress
    let ids_len = ids_buf.count
    // ... call C function with ids_ptr, ids_len ...
}

// List return: -> [Int32]
var outLen: Int = 0
let rv = weaveffi_batch_get_ids(&outLen, &err)
try check(&err)
guard let rv = rv else { return [] }
return Array(UnsafeBufferPointer(start: rv, count: outLen))
```

### Functions

Module functions are generated as static methods on an enum namespace.
Every function takes a trailing `weaveffi_error*` and the Swift wrapper
calls `try check(&err)` to convert errors to Swift exceptions:

```swift
public enum Contacts {
    public static func create_contact(_ name: String, _ age: Int32) throws -> Contact {
        var err = weaveffi_error(code: 0, message: nil)
        // ... buffer setup for string params ...
        let rv = weaveffi_contacts_create_contact(name_ptr, name_len, age, &err)
        try check(&err)
        guard let rv = rv else {
            throw WeaveFFIError.error(code: -1, message: "null pointer")
        }
        return Contact(ptr: rv)
    }

    public static func find_contact(_ id: Int32) throws -> Contact? {
        var err = weaveffi_error(code: 0, message: nil)
        let rv = weaveffi_contacts_find_contact(id, &err)
        try check(&err)
        return rv.map { Contact(ptr: $0) }
    }

    public static func list_contacts() throws -> [Contact] {
        var err = weaveffi_error(code: 0, message: nil)
        var outLen: Int = 0
        let rv = weaveffi_contacts_list_contacts(&outLen, &err)
        try check(&err)
        guard let rv = rv else { return [] }
        return (0..<outLen).map { Contact(ptr: rv[$0]!) }
    }
}
```

## Try the example app

### macOS

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

### Linux

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

## Integration via SwiftPM

In a real app, add the System Library as a dependency and link it with your
target. The `CWeaveFFI` module map provides header linkage; import `WeaveFFI`
in your Swift code for the ergonomic wrapper.
