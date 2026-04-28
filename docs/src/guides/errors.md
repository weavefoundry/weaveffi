# Error Handling

## Overview

WeaveFFI uses a uniform error model across the FFI boundary. Every
generated function carries an out-error parameter (`weaveffi_error*`)
that reports success or failure through an integer code and an
optional message string. Each generator maps that to its target's
idiomatic error mechanism (exceptions, `throws`, `Result`, etc.) so
consumers rarely touch the C-level struct directly.

## When to use

Reach for this guide when:

- You are designing an IDL and want to surface stable, named error
  codes to consumers.
- You are writing the Rust implementation of a module and need to
  return errors over the C ABI.
- You are debugging an "unknown error" surface in a generated
  binding.
- You are reviewing or extending a generator and need to know what the
  error contract guarantees.

## Step-by-step

### Define an error domain in the IDL

```yaml
version: "0.3.0"
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
      - name: get_contact
        params:
          - { name: id, type: handle }
        return: string
```

The validator enforces:

- `code = 0` is reserved for success; non-zero is required.
- All names within a domain are unique.
- All numeric codes within a domain are unique.
- The domain `name` must not collide with any function name in the
  module.
- The domain `name` must not be empty.

### Set errors from the Rust implementation

```rust
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_contacts_get_contact(
    id: u64,
    out_err: *mut weaveffi_error,
) -> *const std::ffi::c_char {
    abi::error_set_ok(out_err);
    abi::error_set(out_err, 1, "Contact not found");
    std::ptr::null()
}
```

| Helper                                 | Effect                                              |
|----------------------------------------|-----------------------------------------------------|
| `error_set_ok(out_err)`                | Sets `code = 0`, frees any prior message            |
| `error_set(out_err, code, msg)`        | Sets a non-zero code and allocates a message        |
| `result_to_out_err(result, out_err)`   | Maps `Result<T, E>` (Ok clears, Err sets `-1`)      |

Prefer the codes you defined in the IDL (e.g. `not_found = 1`) so
consumers can react meaningfully.

### Handle errors in C

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

The pattern is always:

1. Zero-initialise: `weaveffi_error err = {0, NULL};`.
2. Call the function with `&err` as the last argument.
3. Check `err.code`; if non-zero, read `err.message` and call
   `weaveffi_error_clear(&err)`.
4. Reuse the struct for subsequent calls.

### Handle errors in Swift

```swift
do {
    let contact = try Contacts.getContact(id: handle)
    print(contact)
} catch let e as WeaveFFIError {
    print("Failed: \(e)")
}
```

The generated wrapper calls `try check(&err)` after every C call,
which throws `WeaveFFIError` and clears the C-side struct.

### Handle errors in Kotlin / Android

```kotlin
try {
    val contact = Contacts.getContact(id)
    println(contact)
} catch (e: RuntimeException) {
    println("Failed: ${e.message}")
}
```

The JNI shim throws `RuntimeException` with the message and clears the
C-side struct before returning.

### Handle errors in Node.js

```typescript
import { Contacts } from "weaveffi";

try {
    const contact = Contacts.getContact(id);
    console.log(contact);
} catch (e) {
    console.error("Failed:", (e as Error).message);
}
```

The N-API addon throws a JavaScript `Error` carrying the message.

### Handle errors in WASM

The minimal WASM target uses numeric return codes. Inspect the return
value after each call:

```javascript
const result = instance.exports.weaveffi_contacts_get_contact(id);
if (result === 0) {
    console.error("call failed — inspect log");
}
```

The WASM error surface is still evolving. Future versions will surface
richer error information.

## Reference

| Layer        | Error mechanism                          | How a non-zero code surfaces                   |
|--------------|------------------------------------------|-----------------------------------------------|
| C ABI        | `weaveffi_error { code, message }`        | Consumer inspects struct after every call      |
| Swift        | `WeaveFFIError` (`throws`)                | `try` raises a Swift `Error`                   |
| Kotlin       | `RuntimeException`                        | `try`/`catch` (or rethrown by the JNI shim)    |
| Node.js      | JavaScript `Error`                        | N-API addon throws                             |
| Python       | `WeaveffiError` exception                 | `try`/`except`                                 |
| Ruby         | `WeaveFFI::Error` (`StandardError`)        | `begin`/`rescue`                              |
| Dart         | `WeaveffiException`                       | `try`/`on WeaveffiException catch`            |
| .NET         | `WeaveffiException`                       | `try`/`catch`                                  |
| Go           | `error` return value                      | Standard `if err != nil { ... }`               |
| WASM         | Numeric return code                       | Caller checks the value                        |

| Field     | Type           | Description                                       |
|-----------|----------------|---------------------------------------------------|
| `code`    | `int32_t`      | `0` = success, non-zero = error                   |
| `message` | `const char*`  | `NULL` on success; Rust-allocated string on error |

See the [Memory Ownership Guide](memory.md) for the freeing contract
on `err.message`.

## Pitfalls

- **Forgetting to call `weaveffi_error_clear`** — the message is
  Rust-allocated. Skipping the clear leaks the string.
- **Reading `err.message` after clearing** — the pointer is invalid as
  soon as `weaveffi_error_clear` returns.
- **Using `code = 0` as a domain value** — the validator rejects this
  because `0` always means success.
- **Reusing custom codes across modules and assuming they are
  unique** — error domains are scoped to a single module. Document
  cross-module conventions if you need them.
- **Not initialising the struct** — always start with
  `{0, NULL}` (or the language equivalent). Stale `code` values from
  earlier calls produce confusing failures.
- **Ignoring the return value when `code != 0`** — Rust does not
  promise the return value is meaningful on failure. For pointer
  returns it is typically `NULL`; do not free it.
