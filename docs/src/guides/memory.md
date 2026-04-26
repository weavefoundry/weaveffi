# Memory Ownership Guide

WeaveFFI exposes Rust functionality through a stable C ABI. Because Rust and C
(and Swift, Kotlin, etc.) have fundamentally different memory models, every
allocation that crosses the FFI boundary follows strict ownership rules.

**Golden rule:** whoever allocates the memory owns it, and ownership must be
explicitly transferred back for deallocation. Rust allocates; the caller frees
through the designated `weaveffi_free_*` functions.

## Allocator contract

WeaveFFI has **one heap** that matters across the FFI boundary: Rust's. Rust
and the consumer (C, Swift, Kotlin, Node.js, .NET, Python, Dart, Go, Ruby,
WASM) may each link against a different system allocator — `malloc` on POSIX,
`HeapAlloc` on Windows, `CoTaskMemAlloc` in the CLR, the JavaScript runtime's
own heap, and so on. Freeing a pointer with a different allocator than the one
that produced it corrupts the heap.

To make ownership unambiguous, every cross-boundary heap allocation is routed
through the WeaveFFI ABI runtime. Two rules apply, without exception:

1. **Rust-allocated pointers must be freed by Rust.** Use `weaveffi_free`,
   `weaveffi_free_string`, `weaveffi_free_bytes`, `weaveffi_error_clear`, or
   the struct-specific `*_destroy` function. The consumer's system allocator
   must **NEVER** free a Rust-allocated pointer — no `free`, no `delete`, no
   `HeapFree`, no `Marshal.FreeCoTaskMem`, no language-runtime finalizer that
   routes through the consumer heap.
2. **Consumer-allocated pointers must be freed by the consumer.** Rust must
   **NEVER** free a pointer it did not allocate. When the consumer needs a
   raw byte buffer that Rust will later release (or vice versa), allocate it
   with `weaveffi_alloc` so the storage comes from Rust's heap from the start.

Violating either rule is undefined behavior: at best a silent leak, at worst
a heap-corruption crash on the next unrelated allocation.

### `weaveffi_alloc` and `weaveffi_free` (C signatures)

The C header declares these two prototypes next to `weaveffi_free_string` /
`weaveffi_free_bytes`:

```c
// Allocate `size` bytes from Rust's heap (alignment 1).
// Returns NULL when `size` is 0 or allocation fails.
uint8_t* weaveffi_alloc(size_t size);

// Free a buffer previously returned by weaveffi_alloc. `size` must match
// the original allocation. A NULL ptr or 0 size is a no-op, so consumers
// can safely forward defaults.
void weaveffi_free(uint8_t* ptr, size_t size);
```

Use them whenever you need a raw byte buffer that crosses the boundary and
is not already covered by a typed helper:

```c
uint8_t* scratch = weaveffi_alloc(1024);
if (!scratch) {
    return 1;
}
// ... hand `scratch` to Rust, or fill it yourself ...
weaveffi_free(scratch, 1024);   // REQUIRED — do NOT call free()
```

For the common typed cases, prefer the dedicated helpers documented in the
rest of this guide (`weaveffi_free_string` for returned `const char*`,
`weaveffi_free_bytes` for returned `(ptr, len)` pairs, `*_destroy` for opaque
structs, `weaveffi_error_clear` for error messages).

## String ownership

Rust-returned strings are NUL-terminated, UTF-8, heap-allocated C strings
created via `CString::into_raw`. The caller **must** free them with
`weaveffi_free_string` after use.

### C

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
weaveffi_free_string(echoed);   // REQUIRED — Rust allocated this
```

### Swift

```swift
var err = weaveffi_error(code: 0, message: nil)
let raw = weaveffi_calculator_echo(
    Array("hello".utf8), 5, &err)
// ... check err ...

if let raw = raw {
    let result = String(cString: raw)
    weaveffi_free_string(raw)   // REQUIRED — Rust allocated this
    print(result)
}
```

The generated Swift wrapper handles this automatically with `defer`:

```swift
let raw = weaveffi_calculator_echo(...)
defer { weaveffi_free_string(raw) }
return String(cString: raw!)
```

### Common mistakes

```c
// BUG: use-after-free — reading string after freeing it
const char* name = weaveffi_contacts_Contact_get_name(contact);
weaveffi_free_string(name);
printf("name: %s\n", name);    // UNDEFINED BEHAVIOR

// BUG: double-free — freeing the same pointer twice
const char* s = weaveffi_calculator_echo((const uint8_t*)"hi", 2, &err);
weaveffi_free_string(s);
weaveffi_free_string(s);       // UNDEFINED BEHAVIOR

// BUG: memory leak — forgetting to free
const char* s = weaveffi_calculator_echo((const uint8_t*)"hi", 2, &err);
printf("%s\n", s);
// missing weaveffi_free_string(s) — memory leaked
```

## Byte buffer ownership

Byte buffers (`bytes` type) are returned as a `const uint8_t*` with a
separate `size_t* out_len` output parameter. The caller **must** free them
with `weaveffi_free_bytes(ptr, len)`.

### C

```c
size_t out_len = 0;
const uint8_t* buf = weaveffi_module_get_data(&out_len, &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

// Copy what you need before freeing
process_data(buf, out_len);
weaveffi_free_bytes((uint8_t*)buf, out_len);  // REQUIRED
```

### Swift

```swift
var outLen: Int = 0
let raw = weaveffi_module_get_data(&outLen, &err)
guard let raw = raw else { return Data() }
defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: raw), outLen) }
let data = Data(bytes: raw, count: outLen)
```

### Common mistakes

```c
// BUG: wrong length — passing incorrect length to free_bytes
size_t len = 0;
const uint8_t* buf = weaveffi_module_get_data(&len, &err);
weaveffi_free_bytes((uint8_t*)buf, 0);    // WRONG length — undefined behavior

// BUG: forgetting to free
size_t len = 0;
const uint8_t* buf = weaveffi_module_get_data(&len, &err);
// missing weaveffi_free_bytes — memory leaked
```

## Struct lifecycle

Structs are opaque on the C side. Their lifecycle follows a strict pattern:

1. **`_create`** allocates and returns a pointer. Caller owns it.
2. **`_destroy`** frees the struct. Must be called exactly once.
3. **`_get_*`** getters read fields. Primitive getters (i32, f64, bool) return
   values directly — no memory management needed. String and bytes getters
   return **new owned copies** that the caller must free separately.

### C

```c
weaveffi_error err = {0, NULL};

// 1. Create — caller now owns the struct
weaveffi_contacts_Contact* contact = weaveffi_contacts_Contact_create(
    (const uint8_t*)"Alice", 5,
    (const uint8_t*)"alice@example.com", 17,
    30,
    &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

// 2. Read fields — primitive getter, no free needed
int32_t age = weaveffi_contacts_Contact_get_age(contact);
printf("age: %d\n", age);

// 3. Read fields — string getter returns owned copy, must free
const char* name = weaveffi_contacts_Contact_get_name(contact);
printf("name: %s\n", name);
weaveffi_free_string(name);   // free the getter's returned string

// 4. Destroy — frees the struct itself
weaveffi_contacts_Contact_destroy(contact);
```

### Swift

The generated Swift wrapper wraps the opaque pointer in a class whose `deinit`
calls `_destroy` automatically:

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

    public var age: Int32 {
        return weaveffi_contacts_Contact_get_age(ptr)
    }
}
```

Swift's ARC ensures `deinit` runs when the last reference is dropped. String
getters use `defer { weaveffi_free_string(...) }` to free after copying into a
Swift `String`.

### Common mistakes

```c
// BUG: use-after-free — accessing struct after destroying it
weaveffi_contacts_Contact_destroy(contact);
int32_t age = weaveffi_contacts_Contact_get_age(contact);  // UNDEFINED BEHAVIOR

// BUG: double-free — destroying twice
weaveffi_contacts_Contact_destroy(contact);
weaveffi_contacts_Contact_destroy(contact);  // UNDEFINED BEHAVIOR

// BUG: leaking getter string — getter returns owned copy
const char* name = weaveffi_contacts_Contact_get_name(contact);
// missing weaveffi_free_string(name) — leaked

// BUG: memory leak — forgetting to destroy
weaveffi_contacts_Contact* c = weaveffi_contacts_Contact_create(...);
// missing weaveffi_contacts_Contact_destroy(c) — struct leaked
```

## Error struct lifecycle

Every FFI function takes a trailing `weaveffi_error* out_err`. On failure,
Rust writes into `out_err->code` (non-zero) and `out_err->message` (a
Rust-allocated C string). Clearing the error frees the message.

### C

```c
weaveffi_error err = {0, NULL};  // stack-allocated, zero-initialized

int32_t result = weaveffi_calculator_div(10, 0, &err);
if (err.code) {
    fprintf(stderr, "error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);  // frees err.message, zeroes fields
}

// err is now safe to reuse for the next call
result = weaveffi_calculator_add(1, 2, &err);
```

### Swift

The generated Swift wrapper provides a `check` helper that copies the error
message, clears the C error, and throws a Swift error:

```swift
var err = weaveffi_error(code: 0, message: nil)
let result = weaveffi_calculator_div(10, 0, &err)
try check(&err)  // throws WeaveFFIError, calls weaveffi_error_clear internally
```

### Common mistakes

```c
// BUG: leaking error message — forgetting to clear
weaveffi_error err = {0, NULL};
weaveffi_calculator_div(1, 0, &err);
if (err.code) {
    fprintf(stderr, "error: %s\n", err.message);
    // missing weaveffi_error_clear(&err) — err.message leaked
}

// BUG: use-after-free — reading message after clearing
weaveffi_error err = {0, NULL};
weaveffi_calculator_div(1, 0, &err);
if (err.code) {
    weaveffi_error_clear(&err);
    printf("%s\n", err.message);  // UNDEFINED BEHAVIOR — message was freed
}
```

## Thread safety

All WeaveFFI-generated FFI functions are expected to be called from a
**single thread** unless the module documentation explicitly states otherwise.

Concurrent calls from multiple threads into the same module may cause data
races and undefined behavior. If you need multi-threaded access, synchronize
externally (e.g., with a mutex or serial dispatch queue) on the calling side.

```c
// CORRECT — all calls on the main thread
int32_t a = weaveffi_calculator_add(1, 2, &err);
int32_t b = weaveffi_calculator_mul(3, 4, &err);
```

```swift
// CORRECT — serialize access through a serial queue
let queue = DispatchQueue(label: "com.app.weaveffi")
queue.sync {
    let result = try? Calculator.add(a: 1, b: 2)
}
```

## Summary

| Resource           | Allocator | Free function              | Notes                                |
|--------------------|-----------|----------------------------|--------------------------------------|
| Raw byte buffer    | Rust      | `weaveffi_free`            | Pair with `weaveffi_alloc`; size must match |
| Returned string    | Rust      | `weaveffi_free_string`     | Every `const char*` return           |
| Returned bytes     | Rust      | `weaveffi_free_bytes`      | Pass both pointer and length         |
| Struct instance    | Rust      | `*_destroy`                | Call exactly once                    |
| String from getter | Rust      | `weaveffi_free_string`     | Getter returns an owned copy         |
| Error message      | Rust      | `weaveffi_error_clear`     | Clears code and frees message        |
