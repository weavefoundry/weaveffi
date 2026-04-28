# Memory Ownership

## Overview

WeaveFFI exposes Rust functionality through a stable C ABI. Because
Rust and the consumer languages (C, Swift, Kotlin, Python, ...) have
different memory models, every allocation that crosses the boundary
follows strict ownership rules.

**Golden rule:** whoever allocates owns it, and ownership must be
explicitly transferred back for deallocation. Rust allocates; the
consumer frees through the designated `weaveffi_free_*` functions or
the matching `_destroy` symbol.

## When to use

Read this guide when:

- You are writing a consumer in C/C++ where the compiler will not free
  anything for you.
- You are debugging a leak, double-free, or use-after-free in a
  generated binding.
- You are extending a generator and need to verify the ownership
  contract for a new type.
- You are reviewing PRs that add new IDL types that involve
  heap-allocated data.

For higher-level languages (Swift, Kotlin, Python, .NET, Dart, Ruby,
Go) the generated wrappers handle most of this automatically; the rules
below explain what those wrappers are doing under the hood.

## Step-by-step

### Strings

Rust returns NUL-terminated, UTF-8, heap-allocated C strings created
via `CString::into_raw`. The consumer must free them with
`weaveffi_free_string`.

```c
weaveffi_error err = {0, NULL};
const char* echoed = weaveffi_calculator_echo(
    (const uint8_t*)"hello", 5, &err);
if (err.code) {
    fprintf(stderr, "%s\n", err.message);
    weaveffi_error_clear(&err);
    return 1;
}

printf("result: %s\n", echoed);
weaveffi_free_string(echoed);
```

Generated wrappers do the same with `defer`:

```swift
let raw = weaveffi_calculator_echo(...)
defer { weaveffi_free_string(raw) }
return String(cString: raw!)
```

### Byte buffers

Byte buffers are returned as `const uint8_t*` plus an `out_len`. Free
them with `weaveffi_free_bytes(ptr, len)` — the length must match what
the C ABI returned.

```c
size_t out_len = 0;
const uint8_t* buf = weaveffi_module_get_data(&out_len, &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

process_data(buf, out_len);
weaveffi_free_bytes((uint8_t*)buf, out_len);
```

### Struct lifecycle

Structs are opaque on the consumer side. The lifecycle is:

1. `*_create` allocates and returns a pointer; the consumer owns it.
2. `*_destroy` frees the struct. Call exactly once.
3. `*_get_<field>` getters read fields. Primitive getters (`i32`,
   `f64`, `bool`) return values directly. String/bytes getters return
   **new owned copies** that must be freed.

```c
weaveffi_error err = {0, NULL};

weaveffi_contacts_Contact* contact = weaveffi_contacts_Contact_create(
    (const uint8_t*)"Alice", 5,
    (const uint8_t*)"alice@example.com", 17,
    30,
    &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

int32_t age = weaveffi_contacts_Contact_get_age(contact);
const char* name = weaveffi_contacts_Contact_get_name(contact);
weaveffi_free_string(name);

weaveffi_contacts_Contact_destroy(contact);
```

The generated Swift wrapper invokes `_destroy` from `deinit` and frees
returned strings with `defer`:

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

### Error struct lifecycle

Every C ABI function takes a trailing `weaveffi_error* out_err`. On
failure Rust writes a non-zero `code` and a Rust-allocated `message`.
Clearing the error frees the message:

```c
weaveffi_error err = {0, NULL};

int32_t result = weaveffi_calculator_div(10, 0, &err);
if (err.code) {
    fprintf(stderr, "error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
}

result = weaveffi_calculator_add(1, 2, &err);
```

Generated wrappers convert non-zero codes into language-native
exceptions (`throw`, `raise`, `Result::Err`).

### Thread safety

Generated FFI functions are expected to be called from a **single
thread** unless the module's documentation says otherwise. Concurrent
calls from multiple threads can cause data races and undefined
behaviour. Synchronise externally — for example with a mutex or a
serial dispatch queue:

```swift
let queue = DispatchQueue(label: "com.app.weaveffi")
queue.sync {
    let result = try? Calculator.add(a: 1, b: 2)
}
```

## Reference

| Resource           | Allocator | Free function              | Notes                                |
|--------------------|-----------|----------------------------|--------------------------------------|
| Returned string    | Rust      | `weaveffi_free_string`     | Every `const char*` return           |
| Returned bytes     | Rust      | `weaveffi_free_bytes`      | Pass both pointer and length         |
| Struct instance    | Rust      | `*_destroy`                | Call exactly once                    |
| String from getter | Rust      | `weaveffi_free_string`     | Getter returns an owned copy         |
| Error message      | Rust      | `weaveffi_error_clear`     | Clears code and frees message        |

## Pitfalls

- **Use-after-free** — reading a string after freeing it, or accessing
  a struct after `_destroy`. Once the consumer frees something, the
  pointer is invalid.
- **Double-free** — freeing the same pointer twice (e.g. calling
  `weaveffi_free_string` twice or invoking `_destroy` after the wrapper
  has already done so).
- **Wrong length to `weaveffi_free_bytes`** — always free with the
  exact length the C ABI returned in `out_len`.
- **Forgetting to clear error structs** — `err.message` is
  Rust-allocated; failing to call `weaveffi_error_clear` after a
  non-zero code leaks that string.
- **Calling FFI from multiple threads without synchronisation** — the
  default contract is single-threaded; synchronise externally if you
  need parallelism.
- **Manually freeing pointers passed in as borrowed parameters** —
  borrowed inputs (`&str`, `&[u8]`, `const T*`) are owned by the
  caller and must not be passed to `weaveffi_free_*`.
