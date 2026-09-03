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
experience are producer bugs (a panic, a marshalling failure) or a failing
consumer callback, and those trap loudly through the target's
programming-error idiom rather than surfacing as a typed error. The two
interpretations are named once, in `weaveffi_core::plan::ErrorStrategy`, and
every generator renders the same pair; see
[Throws versus Trap](#throws-versus-trap).

Underneath, every generated symbol still reports through the C-level
out-error parameter (`weaveffi_error*`) with an integer code, an optional
message string, and an optional structured payload; the typed surface is
built on top of it. ABI revision 2 adds one code to the reserved negative
range, `-4` (`FOREIGN_ERROR_CODE`), for failures that originate in a
consumer's callback-interface implementation, and one runtime symbol,
`weaveffi_error_set`, that consumers use to report them.

## When to use

Reach for this guide when:

- You are designing an IDL and want to surface stable, named error codes to
  consumers as typed errors.
- You are writing the Rust implementation of a module and need to return
  errors over the C ABI.
- You implement a callback interface in a consumer language and want to know
  what happens when your implementation throws.
- You are debugging an "unknown error" surface in a generated binding.
- You are reviewing or extending a generator and need to know what the error
  contract guarantees.

## Step-by-step

### Declare a domain and opt in with `throws`

```yaml
version: "0.9.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - name: KeyNotFound
          code: 1001
          message: "key not found"
        - name: Expired
          code: 1002
          message: "entry expired"
        - name: StoreFull
          code: 1003
          message: "store has reached capacity"
        - name: IoError
          code: 1004
          message: "I/O failure"

    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
            throws: true
        methods:
          - name: get
            params:
              - { name: key, type: string }
            return: Entry?
            throws: true
          - name: count
            params: []
            return: i64
```

`open` and `get` are fallible and deliver `KvError` values; `count` has a
plain signature in every target. Code names are PascalCase by convention
(`KeyNotFound`, not `key_not_found`); each generator re-cases them into its
own idiom.

The domain is in scope for its module and every module nested inside it, so
one domain on a parent module can serve a whole subtree. Free functions,
interface constructors, methods, and statics all opt in with the same
`throws: true` flag. Callback-interface methods can't: they are implemented
by the consumer, and a consumer failure has its own channel (see
[Foreign errors](#foreign-errors)).

A code may additionally declare structured payload `fields:` (the same shape
as record fields). When a matching error is raised, those fields travel
across the ABI in the error's payload buffer and surface as properties on the
typed error the consumer catches:

```yaml
codes:
  - name: StoreFull
    code: 1003
    message: "store has reached capacity"
    fields:
      - { name: capacity, type: i64 }
```

See [Structured error payloads](../reference/idl.md#structured-error-payloads)
for the schema and the
[Value Buffer Protocol](../reference/value-buffers.md#structured-errors) for
the wire format.

The validator enforces:

- Codes must be positive. `0` is reserved for success and the whole negative
  range belongs to the runtime (`-1` generic error, `-2` producer panic, `-3`
  marshalling failure, `-4` foreign error).
- Numeric codes are unique within a domain.
- Code names are unique within a domain **and across every domain in the
  API**. Backends with flat namespaces derive one error class or constant per
  code, so two domains both declaring `NotFound` would collide; qualify one of
  them (for example `OrderNotFound`).
- The domain `name` must not be empty, must not collide with any function
  name in the module, and shares the API-wide type namespace with record,
  enum, interface, and callback-interface names.
- `throws: true` with no domain in scope (on the module or an ancestor) is an
  error, and so is `throws: true` on a callback-interface method.

### Report errors from the producer

**With the Rust macro**, declare the domain as a `#[weaveffi::error]` enum
whose discriminants are the ABI codes (doc comments become the default
messages), and return `Result<T, YourError>` from fallible functions. This is
the `kvstore` sample's domain:

```rust
#[weaveffi::module]
pub mod kv {
    /// The store's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum KvError {
        /// key not found
        KeyNotFound = 1001,
        /// entry expired
        Expired = 1002,
        /// store has reached capacity
        StoreFull = 1003,
        /// I/O failure
        IoError = 1004,
    }

    #[weaveffi::export]
    pub fn open_path(path: String) -> Result<String, KvError> {
        if path.is_empty() {
            return Err(KvError::IoError);
        }
        Ok(path)
    }
}

weaveffi::export_runtime!();
```

The macro generates the `ErrorReport` implementation and the C ABI thunks
that write the matching code and message into `out_err`. `Result<T, String>`
is also accepted; it reports the generic code `-1` with the string as the
message, which consumers see as the untyped fallback rather than a domain
case.

A variant may carry named fields, which become the code's structured payload;
the macro serializes them into the error's payload buffer in declaration
order. Field-carrying variants with explicit discriminants require a
primitive repr on the enum:

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum QuotaError {
    /// quota exceeded
    Exceeded { limit: i64, used: i64 } = 3001,
    /// quota service unavailable
    Unavailable = 3002,
}
```

**If you hand-implement the C ABI** (a non-Rust producer, or Rust without the
macro), report through the `weaveffi-abi` helpers, preferring the codes you
declared in the IDL:

```rust
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_kv_open_path(
    path: *const std::ffi::c_char,
    out_err: *mut weaveffi_error,
) -> *const std::ffi::c_char {
    let _ = path;
    abi::error_set(out_err, 1004, "I/O failure");
    std::ptr::null()
}
```

| Helper | Effect |
|--------|--------|
| `error_set_ok(out_err)` | Sets `code = 0`, frees any prior message and payload |
| `error_set(out_err, code, msg)` | Sets a non-zero code and allocates a message |
| `error_set_with_payload(out_err, code, msg, payload)` | Like `error_set`, plus an owned payload buffer (the code's fields in value-buffer format) |
| `result_to_out_err(result, out_err)` | Maps `Result<T, E>` through `ErrorReport` (domain code, message, and payload for implementors; generic `-1` for `String` and `&str` errors) |
| `error_set_panic(out_err, payload)` | Reports a caught unwind: `-4` with the consumer's message for a `ForeignError` payload, `-2` for anything else |
| `check_foreign_error(err)` | After a vtable call: no-op on `code == 0`, otherwise unwinds with a `ForeignError` |

Hand-written `ErrorReport` implementations can override the trait's
`payload(&self) -> Vec<u8>` method to serialize a code's declared fields; the
default reports no payload.

### Handle errors in C

The C surface is the raw out-error struct:

```c
weaveffi_error err = {0};

weaveffi_kv_Store* store = weaveffi_kv_Store_open("", &err);
if (err.code) {
    fprintf(stderr, "error %d: %s\n", err.code,
            err.message ? err.message : "unknown");
    weaveffi_error_clear(&err);
    return 1;
}

weaveffi_kv_Store_destroy(store);
```

The pattern is always:

1. Zero-initialise: `weaveffi_error err = {0};`.
2. Call the function with `&err` as the last argument.
3. Check `err.code`; if non-zero, read `err.message` (and `payload_ptr` /
   `payload_len` if the code declares fields) and call
   `weaveffi_error_clear(&err)`.
4. Reuse the struct for subsequent calls.

The domain's codes are also emitted as a C enum, so a consumer can match on
names instead of magic numbers:

```c
typedef enum {
    weaveffi_kv_KvError_KeyNotFound = 1001,
    weaveffi_kv_KvError_Expired = 1002,
    weaveffi_kv_KvError_StoreFull = 1003,
    weaveffi_kv_KvError_IoError = 1004
} weaveffi_kv_KvError;
```

Async completion callbacks receive a *heap-boxed* `weaveffi_error*` instead
of writing into a caller-owned slot; release it with `weaveffi_error_free`,
which clears the slot and frees the box. See
[Async Functions](async.md#result-ownership-and-threading).

### What consumers see

Every other target wraps that struct into a typed error construct named after
the domain. The table below is taken from the bindings `weaveffi generate`
emits for `samples/kvstore` (domain `KvError`, code `KeyNotFound = 1001`):

| Target | Domain type | The `KeyNotFound` case | Generic fallback |
|--------|-------------|------------------------|------------------|
| Swift | `enum KvError: Error, LocalizedError` | `.keyNotFound(message:)` | `WeaveFFIError.error(code:message:)` |
| Kotlin | `sealed class KvException : WeaveFFIException` | `KvException.KeyNotFound` | `WeaveFFIException(code, message)` |
| Python | `class KvError(WeaveFFIError)` | `class KeyNotFound(KvError)` | `WeaveFFIError` |
| Node.js | `class KvError extends WeaveFFIError` | `class KeyNotFoundError extends KvError` (`KeyNotFoundError.CODE === 1001`) | `WeaveFFIError` |
| WASM | `class KvError extends WeaveFFIError` | `class KeyNotFound extends KvError` (`KeyNotFound.CODE === 1001`) | `WeaveFFIError` |
| .NET | `class KvException : WeaveFFIException` | `KvException.KeyNotFound` constant, matched on `Code` | `WeaveFFIException` |
| Dart | `class KvException extends WeaveFFIException` | `class KeyNotFoundException extends KvException` | `WeaveFFIException` |
| Ruby | `class KvError < Kvstore::Error` | `KvError::KeyNotFound` | `Kvstore::Error` (the wrapping module is `[generators.ruby] module_name`, `WeaveFFI` by default; the sample sets `Kvstore`) |
| Go | `type KvError struct { Code, Message }` | `KvErrorKeyNotFound` code constant | `*WeaveFFIError` |
| C++ | `class KvError : public WeaveFFIError` | `class KeyNotFoundError : public KvError` | `kvstore::WeaveFFIError` |

In Swift, throwing wrappers `throw` the domain enum:

```swift
do {
    let store = try Store.open(path: "")
} catch KvError.ioError(let message) {
    print("cannot open: \(message)")
}
```

In Python, the domain is an exception class (subclassing the generic
`WeaveFFIError`) with one subclass per code carrying its stable `CODE`:

```python
try:
    store = Store.open("")
except IoError as e:
    print("cannot open:", e)
```

The remaining targets follow the same conceptual shape in their own idiom:
one typed error construct per domain, delivered through the language's native
error channel. Ecosystems that suffix exceptions rename the domain
accordingly (`KvError` becomes `KvException` in Kotlin, .NET, and Dart, and
`IoError` becomes `IoException`). A code the consumer doesn't recognize (from
a newer producer, for example) falls back to the generic branded error rather
than being dropped.

When a code declares payload `fields:`, each generator decodes the error's
payload buffer and exposes the fields as properties of the raised exception
(or returned error value), keyed by the field names.

### Foreign errors

A callback interface is implemented by the consumer, so its methods can fail
in the consumer's language: a Swift `throw`, a Python exception, a Go panic.
A callback method has no `throws` flag and returns only a scalar, so that
failure travels the other way through the ABI:

1. The generated trampoline catches the failure and reports it into the
   vtable entry's `out_err` slot by calling
   `weaveffi_error_set(out_err, -4, message)`. The runtime copies the
   borrowed message with the producer's allocator, so the trampoline can free
   its own copy immediately.
2. On the producer side, the generated trait implementation calls
   `check_foreign_error` after every vtable call. A non-zero code unwinds the
   producer's current call with a `ForeignError` payload (via
   `resume_unwind`, so the panic hook does not fire; this is control flow, not
   a bug report). A negative code is kept as written; a positive one is
   replaced by `-4`, so a consumer bug can't masquerade as one of the
   producer's domain codes. On a `panic = "abort"` build (notably
   `wasm32-unknown-unknown`) there's no unwinding: the failure is recorded in
   a thread-local slot, the vtable entry's zero return value flows back into
   the producer's code, and the thunk picks the recorded failure up with
   `take_foreign_error` once the producer returns.
3. The enclosing thunk reports `FOREIGN_ERROR_CODE` (`-4`) with the
   consumer's message to the **original caller**, the code that invoked the
   producer function that in turn invoked the callback.

Every generated trampoline already does step 1; the table shows what your
implementation writes and what the original caller reads:

| Target | How an implementation fails | What the original caller sees |
|--------|-----------------------------|-------------------------------|
| Swift | protocol method is `throws`; throw any `Error` | `WeaveFFIError.error(code: -4, ...)` on a throwing call; `fatalError` on a non-throwing call |
| Kotlin | throw any `Throwable` | `WeaveFFIException` with `code == -4` |
| Python | raise any exception | `WeaveFFIError` with `code == -4` (`WeaveFFIError.FOREIGN_ERROR_CODE`) |
| Node.js / WASM | throw | `WeaveFFIError` with `code === -4` |
| .NET | throw | `WeaveFFIException` with `Code == WeaveFFIException.ForeignErrorCode` |
| Dart | throw | `WeaveFFIException` with `code == WeaveFFIException.foreignCode` |
| Ruby | raise | `Kvstore::Error` with `code == -4` (`Kvstore::FOREIGN_ERROR_CODE`) |
| Go | `panic` inside the method | `*WeaveFFIError` with `Code == -4` on a `(T, error)` call; `panic` on a plain call |
| C++ | throw a `std::exception` (or anything; non-standard exceptions get a fixed message) | `WeaveFFIError` with `code() == -4` |
| C | `weaveffi_error_set(out_err, -4, "message")` and return a default | `err.code == -4` |

Because `-4` is not a domain code, the typed path never maps it onto a
domain case: a throwing call surfaces it as the generic branded error, and a
non-throwing call traps. If the producer function that called the callback
was declared without `throws`, the consumer's own exception therefore comes
back as a trap (`fatalError` in Swift, `panic` in Go). Declare `throws: true`
on producer functions that invoke callbacks if you want consumers to be able
to catch their own failures.

A foreign error also unwinds through whatever the producer was holding at the
time. If that was a `std::sync::Mutex` guard, the mutex is poisoned;
producers that call out to consumers should snapshot state under the lock,
release it, and then call. See
[Memory Ownership](memory.md#producer-side-rules).

### Producer panics

Generated Rust thunks wrap the producer call in `catch_unwind`. A panic is
reported through `out_err` with the reserved code `-2`
(`weaveffi_abi::PANIC_ERROR_CODE`) and the message
`producer panicked: <payload>`, so a consumer can always distinguish "the
producer has a bug" from any declared domain error and from a foreign error.
Panics never surface as typed domain errors: on a throwing callable they
arrive as the generic branded error, and on a non-throwing callable they
surface as the target's unrecoverable idiom (a Swift `fatalError`, a Go
`panic`, a generic exception elsewhere).

Object destructors (`{tag}_destroy`) and iterator destructors have no
`out_err` slot; a panic inside a `Drop` implementation is swallowed rather
than unwinding into C. Async functions report panics through the completion
callback's error slot; see [Async Functions](async.md#panic-handling).

## Reference

### Throws versus Trap

Every synchronous C ABI entry point carries a trailing `out_err`, and every
async completion callback carries an `err` slot, regardless of `throws`. What
differs is the *meaning* of a non-zero code, and every backend agrees on it
because the two interpretations are stated once as
`weaveffi_core::plan::ErrorStrategy`:

- **Throws** (`throws: true`): a non-zero code is a typed domain error. The
  wrapper maps the code onto the module's error domain (an exception
  subclass, a Swift `Error` enum case, a Go `error` value) and surfaces it
  through the target's normal error channel so callers can catch and match
  on it. Negative codes fall through to the generic branded error.
- **Trap** (no `throws`): the only way `out_err` reports failure is a
  producer bug or a failing callback (codes `-2`, `-3`, `-4`). The wrapper
  surfaces it through the target's *programming-error* idiom (a Python
  `WeaveFFIError`, a Go `panic`, a Swift `fatalError`, a C# exception). A
  trapped failure is never silently ignored, and it is never dressed up as a
  typed domain error.

The per-target rendering of both strategies is tabulated
[below](#per-target-surface).

At the ABI level, `weaveffi_error.code` means:

| Code | Constant | Meaning |
|------|----------|---------|
| `0` | | Success |
| a declared code | | A typed producer error from the module's domain (always positive) |
| `-1` | `GENERIC_ERROR_CODE` | Untyped producer error (a `Result<T, String>`, or an error type without a domain code) |
| `-2` | `PANIC_ERROR_CODE` | Producer panic, caught by the thunk's `catch_unwind` |
| `-3` | `MARSHAL_ERROR_CODE` | Marshalling failure: a null or invalid argument, a non-UTF-8 string, an out-of-range enum value, or a malformed value buffer |
| `-4` | `FOREIGN_ERROR_CODE` | A consumer callback-interface implementation failed; the message is the consumer's |

Domain codes are validated positive-only, so the two ranges can't collide: on
the typed path a wrapper maps a positive code to the matching declared case of
the domain type and falls back to the generic branded error for any code the
domain doesn't declare (including every negative runtime code). Generated
wrappers also raise the generic branded error themselves, without calling the
producer, for contract violations they detect locally (a disposed object
wrapper, a value that isn't an object of the expected class, a malformed
value buffer).

### Per-target surface

Per target, the two strategies surface as:

| Target | Throws (`throws: true`) | Trap (producer bug or foreign error) |
|--------|-------------------------|--------------------------------------|
| C | `weaveffi_error { code, message, payload_ptr, payload_len }` struct | same struct (code `-2`, `-3`, or `-4`) |
| Swift | `throws`, typed domain enum | `fatalError` |
| Python | `raise`, domain exception subclass | `raise WeaveFFIError` |
| Kotlin | `throw`, typed domain exception | `throw WeaveFFIException` |
| C# | `throw`, typed domain exception | `throw WeaveFFIException` |
| Dart | `throw`, typed domain exception | `throw WeaveFFIException` |
| Node.js / WASM | `throw`, typed domain error | `throw WeaveFFIError` |
| Ruby | `raise`, typed domain error | `raise <Module>::Error` (`WeaveFFI::Error` by default) |
| Go | `(T, error)` return, typed domain error | `panic` |
| C++ | `throw`, typed domain error | `throw <package>::WeaveFFIError` |

All targets share the canonical `WeaveFFI` brand (never the `heck`-derived
`Weaveffi`) for the generic fallback type. Error type names are derived from
a single naming policy: ecosystems that suffix with `Error` (Swift, C++,
Python, Node, Go) use `WeaveFFIError`; ecosystems that suffix with
`Exception` (Kotlin, .NET, Dart) use `WeaveFFIException`; Ruby nests a plain
`Error` inside the configured wrapper module. Per-code names are PascalCased from the
IDL, and domain type names keep exactly one `Error` (or `Exception`) suffix.

| Field | Type | Description |
|-------|------|-------------|
| `code` | `int32_t` | `0` = success, non-zero = error |
| `message` | `const char*` | `NULL` on success; Rust-allocated string on error |
| `payload_ptr` | `const uint8_t*` | `NULL` unless the matched code declares `fields:`; the fields serialized in the value-buffer format |
| `payload_len` | `size_t` | Byte length of `payload_ptr`; `0` when null |

Three runtime symbols operate on the struct:

| Symbol | Who calls it | Effect |
|--------|--------------|--------|
| `weaveffi_error_clear(err)` | consumer, after reading a sync error | frees message and payload, zeroes the slot; idempotent |
| `weaveffi_error_free(err)` | consumer, after reading an async error | clears the slot and frees the heap box it arrived in |
| `weaveffi_error_set(err, code, message)` | consumer, inside a callback-interface trampoline | copies `message` with the producer's allocator and sets `code` |

See the [Memory Ownership Guide](memory.md) for the freeing contract on
`err.message` and the payload.

## Pitfalls

- **Forgetting to call `weaveffi_error_clear`**: the message and the payload
  are Rust-allocated. Skipping the clear leaks them.
- **Reading `err.message` after clearing**: the pointer is invalid as soon as
  `weaveffi_error_clear` returns.
- **Using `0` or a negative number as a domain code**: the validator rejects
  both; `0` always means success and negative codes are reserved for the
  runtime (`-1` generic, `-2` panic, `-3` marshalling failure, `-4` foreign
  error).
- **Reusing a code name in two domains**: code names are unique across the
  whole API, so the validator rejects a second `NotFound`. Qualify one of
  them (`OrderNotFound`).
- **Declaring `throws: true` without a domain in scope**: a throwing callable
  needs an `errors:` block on its module or an ancestor.
- **Declaring `throws: true` on a callback-interface method**: rejected. The
  consumer reports failures through `weaveffi_error_set` and the producer's
  caller sees `-4`.
- **Expecting a typed error from a non-throwing function**: a callable
  without `throws` cannot deliver a domain error; a failure there is a
  producer bug (or a foreign error) and traps through the target's
  programming-error idiom (see [Throws versus Trap](#throws-versus-trap)).
- **Expecting your own callback exception to come back typed**: it surfaces
  as the generic branded error with code `-4`, never as a domain case, and
  only if the producer function you called was declared `throws: true`.
- **Writing `out_err->message` directly from a hand-written C callback**:
  the producer frees that pointer with Rust's allocator. Use
  `weaveffi_error_set`, which copies.
- **Not initialising the struct**: always start with `{0}` (or the language
  equivalent). Stale `code` values from earlier calls produce confusing
  failures.
- **Ignoring the return value when `code != 0`**: Rust does not promise the
  return value is meaningful on failure. For pointer returns it is typically
  `NULL`; do not free it.
