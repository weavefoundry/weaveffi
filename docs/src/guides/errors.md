# Error Handling Guide

WeaveFFI uses a uniform error model across the FFI boundary. Every generated
function carries an out-error parameter that reports success or failure through
an integer code and an optional message string. Each target language maps this
convention to its own idiomatic error mechanism.

## The `weaveffi_error` struct

At the C ABI level, errors are represented by a simple struct:

```c
typedef struct weaveffi_error {
    int32_t     code;
    const char* message;
} weaveffi_error;
```

| Field     | Type           | Description                                      |
|-----------|----------------|--------------------------------------------------|
| `code`    | `int32_t`      | `0` = success, non-zero = error                  |
| `message` | `const char*`  | `NULL` on success; Rust-allocated string on error |

Every generated C function accepts a trailing `weaveffi_error* out_err`
parameter. On success the runtime sets `code = 0` and `message = NULL`. On
failure it sets a non-zero code and writes a human-readable message.

**Key rule:** `code = 0` always means success. Any non-zero value is an error.
The runtime uses `-1` as the default unspecified error code when no domain code
applies.

After reading the error message you **must** call `weaveffi_error_clear` to
free the Rust-allocated string. See the
[Memory Ownership Guide](memory.md#error-struct-lifecycle) for details.

## Defining error domains in the IDL

You can declare an error domain on a module to assign symbolic names and stable
numeric codes to expected failure conditions. Error domains are optional — if
omitted, errors still work but use the default code `-1`.

### YAML syntax

```yaml
version: "0.1.0"
modules:
  - name: contacts
    errors:
      name: ContactErrors
      codes:
        - name: not_found
          code: 1
          message: "Contact not found"
        - name: duplicate
          code: 2
          message: "Contact already exists"
        - name: invalid_email
          code: 3
          message: "Email address is invalid"

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: string }
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
        return: string
```

### Validation rules

The validator enforces these constraints on error domains:

- **Non-zero codes.** `code = 0` is reserved for success and will be rejected.
- **Unique names.** No two error codes may share the same `name`.
- **Unique numeric codes.** No two error codes may share the same `code` value.
- **No collision with functions.** The error domain `name` must not match any
  function name in the same module.
- **Non-empty name.** The error domain `name` must not be blank.

## How each language maps errors

### C

In C, the caller allocates a `weaveffi_error` on the stack, passes its address,
and checks `code` after the call.

```c
weaveffi_error err = {0, NULL};

const char* contact = weaveffi_contacts_get_contact(id, &err);
if (err.code) {
    fprintf(stderr, "error %d: %s\n", err.code,
            err.message ? err.message : "unknown");
    weaveffi_error_clear(&err);
    return 1;
}

printf("contact: %s\n", contact);
weaveffi_free_string(contact);
```

The pattern is always the same:

1. Zero-initialize: `weaveffi_error err = {0, NULL};`
2. Call the function, passing `&err` as the last argument.
3. Check `err.code` — if non-zero, read `err.message` and clear with
   `weaveffi_error_clear(&err)`.
4. The error struct can be reused for subsequent calls.

### Swift

The generated Swift wrapper defines a `WeaveFFIError` enum and a `check`
helper. Functions are marked `throws` and raise a Swift error automatically
when the C-level code is non-zero.

```swift
public enum WeaveFFIError: Error, CustomStringConvertible {
    case error(code: Int32, message: String)
    public var description: String {
        switch self {
        case let .error(code, message):
            return "(\(code)) \(message)"
        }
    }
}
```

Callers use standard `do`/`catch`:

```swift
do {
    let contact = try Contacts.getContact(id: handle)
    print(contact)
} catch let e as WeaveFFIError {
    print("Failed: \(e)")
}
```

Behind the scenes the generated code initializes a C error, calls the FFI
function, and invokes `check(&err)` which throws if `code != 0`:

```swift
var err = weaveffi_error(code: 0, message: nil)
let raw = weaveffi_contacts_get_contact(id, &err)
try check(&err)  // throws WeaveFFIError, calls weaveffi_error_clear internally
```

### Kotlin / Android

The generated JNI bridge checks the error after each C call and throws a
`RuntimeException` with the error message when `code != 0`.

```kotlin
try {
    val contact = Contacts.getContact(id)
    println(contact)
} catch (e: RuntimeException) {
    println("Failed: ${e.message}")
}
```

In the generated JNI C code:

```c
weaveffi_error err = {0, NULL};
// ... call the C function with &err ...
if (err.code != 0) {
    jclass exClass = (*env)->FindClass(env, "java/lang/RuntimeException");
    const char* msg = err.message ? err.message : "WeaveFFI error";
    (*env)->ThrowNew(env, exClass, msg);
    weaveffi_error_clear(&err);
    return;
}
```

### Node.js

The Node.js generator emits TypeScript type declarations and a loader for a
native N-API addon. Error handling is performed by the addon at runtime — when
the C-level `code != 0`, the addon throws a JavaScript `Error` with the
message.

```typescript
import { Contacts } from "./generated";

try {
    const contact = Contacts.getContact(id);
    console.log(contact);
} catch (e) {
    console.error("Failed:", (e as Error).message);
}
```

### WASM

The WASM generator produces a JavaScript loader that checks the return value
after each call. Error handling at the WASM boundary uses numeric return codes;
the caller inspects the result to determine success or failure.

```javascript
const result = instance.exports.weaveffi_contacts_get_contact(id);
if (result === 0) {
    console.error("call failed — check error output");
}
```

> **Note:** The WASM error surface is still evolving. Future versions will
> provide richer error propagation.

## Setting errors from Rust implementations

When implementing a module in Rust, use the helpers from `weaveffi_abi` to
report success or failure through the out-error pointer:

```rust
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_contacts_get_contact(
    id: u64,
    out_err: *mut weaveffi_error,
) -> *const std::ffi::c_char {
    // Success path — clear the error and return a value
    abi::error_set_ok(out_err);

    // Failure path — set a non-zero code and message
    abi::error_set(out_err, 1, "Contact not found");
    std::ptr::null()
}
```

| Helper                          | Effect                                             |
|---------------------------------|----------------------------------------------------|
| `error_set_ok(out_err)`         | Sets `code = 0`, frees any prior message           |
| `error_set(out_err, code, msg)` | Sets a non-zero code and allocates message          |
| `result_to_out_err(result, out_err)` | Maps `Result<T, E>` — `Ok` clears, `Err` sets code `-1` |

Use error domain codes from your IDL to give callers stable, actionable
values. For example, if your IDL defines `not_found = 1`, call
`error_set(out_err, 1, "Contact not found")`.

## Summary

| Language    | Error mechanism       | How errors surface                                 |
|-------------|-----------------------|----------------------------------------------------|
| C           | Check `code` field    | Caller inspects `err.code` after every call        |
| Swift       | `throws`              | `WeaveFFIError` thrown, caught with `do`/`catch`   |
| Kotlin      | Exception             | `RuntimeException` thrown, caught with `try`/`catch` |
| Node.js     | Thrown `Error`         | Native addon throws JS `Error`                     |
| WASM        | Return code           | Caller checks return value                         |
