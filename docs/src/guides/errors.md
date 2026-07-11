# Error Handling

## Overview

WeaveFFI's error model is typed and opt-in. A module declares an **error
domain**: a named set of symbolic codes. A function, method, or constructor
opts into that domain by declaring `throws: true`, and every generator then
surfaces its failures through the target's idiomatic error mechanism
(`throws` in Swift, `raise` in Python, `(T, error)` in Go, exceptions
elsewhere) carrying a *typed* error derived from the domain, so consumers
catch and match on the codes you declared.

A callable **without** `throws` has a plain signature: no `throws` clause,
no error return. It cannot report a domain error; the only failures it can
experience are producer bugs (a panic, a marshalling failure), and those
trap loudly through the target's programming-error idiom rather than
surfacing as a typed error. The two interpretations are named once, in
`weaveffi_core::plan::ErrorStrategy`, and every generator renders the
same pair; see [Throws versus Trap](#throws-versus-trap).

Underneath, every generated symbol still reports through the C-level
out-error parameter (`weaveffi_error*`) with an integer code and an
optional message string; the typed surface is built on top of it.

## When to use

Reach for this guide when:

- You are designing an IDL and want to surface stable, named error
  codes to consumers as typed errors.
- You are writing the Rust implementation of a module and need to
  return errors over the C ABI.
- You are debugging an "unknown error" surface in a generated
  binding.
- You are reviewing or extending a generator and need to know what the
  error contract guarantees.

## Step-by-step

### Declare a domain and opt in with `throws`

```yaml
version: "0.5.0"
modules:
  - name: contacts
    errors:
      name: ContactsError
      codes:
        - name: InvalidName
          code: 1
          message: "name must not be empty"
        - name: NotFound
          code: 2
          message: "contact not found"

    functions:
      - name: get_contact
        params:
          - { name: id, type: i64 }
        return: string
        throws: true

      - name: count_contacts
        params: []
        return: i32
```

`get_contact` is fallible and delivers `ContactsError` values;
`count_contacts` has a plain signature in every target. Code names are
PascalCase by convention (`NotFound`, not `not_found`); each generator
re-cases them into its own idiom.

The domain is in scope for its module and every module nested inside it,
so one domain on a parent module can serve a whole subtree. Interface
constructors and methods opt in with the same `throws: true` flag.

The validator enforces:

- `code = 0` is reserved for success and `-2` for producer panics;
  any other non-zero value is allowed.
- Numeric codes are unique within a domain.
- Code names are unique within a domain **and across every domain in the
  API**. Backends with flat namespaces derive one error class or constant
  per code, so two domains both declaring `NotFound` would collide;
  qualify one of them (e.g. `OrderNotFound`).
- The domain `name` must not be empty, must not collide with any function
  name in the module, and shares the API-wide type namespace with struct,
  enum, and interface names.
- `throws: true` with no domain in scope (on the module or an ancestor)
  is an error.

### Report errors from the producer

**With the Rust macro**, declare the domain as a `#[weaveffi::error]` enum
whose discriminants are the ABI codes (doc comments become the default
messages), and return `Result<T, YourError>` from fallible functions:

```rust
#[weaveffi::module]
pub mod contacts {
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum ContactsError {
        /// name must not be empty
        InvalidName = 1,
        /// contact not found
        NotFound = 2,
    }

    #[weaveffi::export]
    pub fn get_contact(id: i64) -> Result<String, ContactsError> {
        Err(ContactsError::NotFound)
    }
}
```

The macro generates the `ErrorReport` implementation and the C ABI thunks
that write the matching code and message into `out_err`.

**If you hand-implement the C ABI** (a non-Rust producer, or Rust without
the macro), report through the `weaveffi-abi` helpers, preferring the
codes you declared in the IDL:

```rust
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_contacts_get_contact(
    id: i64,
    out_err: *mut weaveffi_error,
) -> *const std::ffi::c_char {
    abi::error_set(out_err, 2, "contact not found");
    std::ptr::null()
}
```

| Helper                                 | Effect                                              |
|----------------------------------------|-----------------------------------------------------|
| `error_set_ok(out_err)`                | Sets `code = 0`, frees any prior message            |
| `error_set(out_err, code, msg)`        | Sets a non-zero code and allocates a message        |
| `result_to_out_err(result, out_err)`   | Maps `Result<T, E>` through `ErrorReport` (domain code for implementors, generic `-1` for plain `Display` errors) |
| `error_set_panic(out_err, payload)`    | Reports a caught panic with the reserved code `-2`  |

### Handle errors in C

The C surface is the raw out-error struct:

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

The domain's codes are also emitted as a C enum, so a consumer can match
on names instead of magic numbers:

```c
typedef enum {
    weaveffi_contacts_ContactsError_InvalidName = 1,
    weaveffi_contacts_ContactsError_NotFound = 2
} weaveffi_contacts_ContactsError;
```

### What consumers see

Every other target wraps that struct into a typed error construct named
after the domain. In Swift, the domain becomes an error enum with one
case per code (named in lowerCamelCase), and throwing wrappers `throw`
it:

```swift
public enum ContactsError: Error, LocalizedError {
    case invalidName(message: String)
    case notFound(message: String)
}

do {
    let contact = try Contacts.getContact(id: 42)
    print(contact)
} catch ContactsError.notFound {
    print("no such contact")
}
```

In Python, the domain becomes an exception class (subclassing the generic
`WeaveFFIError`) with one subclass per code carrying its stable `CODE`:

```python
try:
    contact = contacts_get_contact(42)
except ContactsError.NotFound:
    print("no such contact")
```

The remaining targets follow the same conceptual shape in their own
idiom: one typed error construct per domain, one case or subclass per
code, delivered through the language's native error channel. Ecosystems
that suffix exceptions rename the domain accordingly (`ContactsError`
becomes `ContactsException` in Kotlin, .NET, and Dart). A code the
consumer doesn't recognize (from a newer producer, for example) falls
back to the generic branded error rather than being dropped.

### Producer panics

Generated Rust thunks wrap the producer call in `catch_unwind`. A panic is
reported through `out_err` with the reserved code `-2`
(`weaveffi_abi::PANIC_ERROR_CODE`) and the panic message, so a consumer
can always distinguish "the producer has a bug" from any declared domain
error. Panics never surface as typed domain errors: on a throwing
callable they arrive as the generic branded error, and on a non-throwing
callable they surface as the target's unrecoverable idiom (a Swift
`fatalError`, a Go `panic`, a generic exception elsewhere).

## Reference

### Throws versus Trap

Every synchronous C ABI entry point carries a trailing `out_err`, and
every async completion callback carries an `err` slot, regardless of
`throws`. What differs is the *meaning* of a non-zero code, and every
backend agrees on it because the two interpretations are stated once as
`weaveffi_core::plan::ErrorStrategy`:

- **Throws** (`throws: true`): a non-zero code is a typed domain error.
  The wrapper maps the code onto the module's error domain (an
  exception subclass, a Swift `Error` enum case, a Go `error` value)
  and surfaces it through the target's normal error channel so callers
  can catch and match on it.
- **Trap** (no `throws`): the only way `out_err` reports failure is a
  producer bug (most commonly a caught panic, code `-2`). The wrapper
  surfaces it through the target's *programming-error* idiom (a Python
  `WeaveFFIError`, a Go `panic`, a Swift `fatalError`, a C# exception).
  A trapped failure is never silently ignored, and it is never dressed
  up as a typed domain error.

The per-target rendering of both strategies is tabulated
[below](#per-target-surface).

At the ABI level, `weaveffi_error.code` means:

| Code               | Meaning                                              |
|--------------------|------------------------------------------------------|
| `0`                | Success                                              |
| a declared code    | A typed producer error from the module's domain      |
| `-1`               | Generic error (null self, bad input, string errors)  |
| `-2`               | Producer panic (`PANIC_ERROR_CODE`)                  |
| `1`                | Invalid argument from marshalling                    |

On the typed path, a wrapper maps a non-zero code to the matching declared
case of the domain type and falls back to the generic branded error for
any code the domain doesn't declare.

### Per-target surface

Per target, the two strategies surface as:

| Target   | Throws (`throws: true`)                     | Trap (producer bug)                      |
|----------|----------------------------------------------|------------------------------------------|
| C        | `weaveffi_error { code, message }` struct    | same struct (code `-2` or `1`)           |
| Swift    | `throws`, typed domain enum                  | `fatalError`                             |
| Python   | `raise`, domain exception subclass           | `raise WeaveFFIError`                    |
| Kotlin   | `throw`, typed domain exception              | `throw WeaveFFIException`                |
| C#       | `throw`, typed domain exception              | `throw WeaveFFIException`                |
| Dart     | `throw`, typed domain exception              | `throw WeaveFFIException`                |
| JS/TS    | `throw`, typed domain error                  | `throw WeaveFFIError`                    |
| Ruby     | `raise`, typed domain error                  | `raise WeaveFFI::Error`                  |
| Go       | `(T, error)` return, typed domain error      | `panic`                                  |
| C++      | `throw`, typed domain error                  | `throw weaveffi::Error`                  |

All targets share the canonical `WeaveFFI` brand (never the `heck`-derived
`Weaveffi`) for the generic fallback type. Error type names are derived
from a single naming policy: ecosystems that suffix with `Error` (Swift,
C++, Python, Node, Ruby, Go) use `WeaveFFIError`; ecosystems that suffix
with `Exception` (Kotlin, .NET, Dart) use `WeaveFFIException`. Per-code
names are PascalCased from the IDL, and domain type names keep exactly one
`Error` (or `Exception`) suffix.

| Field     | Type           | Description                                       |
|-----------|----------------|---------------------------------------------------|
| `code`    | `int32_t`      | `0` = success, non-zero = error                   |
| `message` | `const char*`  | `NULL` on success; Rust-allocated string on error |

See the [Memory Ownership Guide](memory.md) for the freeing contract
on `err.message`.

## Pitfalls

- **Forgetting to call `weaveffi_error_clear`**: the message is
  Rust-allocated. Skipping the clear leaks the string.
- **Reading `err.message` after clearing**: the pointer is invalid as
  soon as `weaveffi_error_clear` returns.
- **Using `code = 0` or `code = -2` as a domain value**: the validator
  rejects both; `0` always means success and `-2` is reserved for
  producer panics.
- **Reusing a code name in two domains**: code names are unique across
  the whole API, so the validator rejects a second `NotFound`. Qualify
  one of them (`OrderNotFound`).
- **Declaring `throws: true` without a domain in scope**: a throwing
  callable needs an `errors:` block on its module or an ancestor.
- **Expecting a typed error from a non-throwing function**: a callable
  without `throws` cannot deliver a domain error; a failure there is a
  producer bug and traps through the target's programming-error idiom
  (see [Throws versus Trap](#throws-versus-trap)).
- **Not initialising the struct**: always start with
  `{0, NULL}` (or the language equivalent). Stale `code` values from
  earlier calls produce confusing failures.
- **Ignoring the return value when `code != 0`**: Rust does not
  promise the return value is meaningful on failure. For pointer
  returns it is typically `NULL`; do not free it.
