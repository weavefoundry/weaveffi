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

The full release contract, exactly which call a wrapper owes after
copying a returned value or a collection element, is stated once, in
`weaveffi_core::plan` (`ReturnFree` for returns, `ElemFree` for
array, map, and iterator elements). Every generated wrapper renders
that plan, and this guide describes the same rules in prose.

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
const char* echoed = weaveffi_calculator_echo("hello", &err);
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
them with `weaveffi_free_bytes(ptr, len)`; the length must match what
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

### Lists, maps, and boxed optionals

Composite returns owe two levels of release: one per element, then one
for the buffer itself.

- **Lists** (`[T]`) return `T* + out_len`. Free each element per its
  element plan (below), then release the array buffer with
  `weaveffi_free_bytes(ptr, len * sizeof(T))`.
- **Maps** (`{K:V}`) return parallel `out_keys` / `out_values` /
  `out_len` buffers. Free each key and each value per its element plan,
  then release both parallel arrays with `weaveffi_free_bytes`.
- **Optional scalars** (`i32?`, `f64?`, ...) return a boxed pointer
  (`T*`, null meaning none). Dereference the value, then release the
  box with `weaveffi_free_bytes(ptr, sizeof(T))`. Optional pointer
  returns (`string?`, `Contact?`) reuse the inner type's plan; a null
  return simply means there is nothing to free.

The per-element plan is:

| Element type                  | Release owed per element             |
|-------------------------------|--------------------------------------|
| Scalar, `bool`, C-style enum, handle | nothing (by value)            |
| `string`                      | `weaveffi_free_string`               |
| Record or rich enum           | the type's `_destroy` symbol (the consumer owns each element) |
| Optional of the above         | the inner plan; skip null slots      |

### Iterator elements

An `iter<T>` return hands the consumer an opaque iterator handle, not a
buffer, so there is nothing to free on launch. Ownership flows per
step:

- Each `_next` call writes an element the consumer now owns. After
  copying it, free it per the element plan above (`weaveffi_free_string`
  for strings, `_destroy` for record or rich-enum elements, nothing for
  by-value elements).
- The handle is released with the iterator's own `_destroy` symbol,
  exactly once: eagerly on exhaustion, and from the wrapper's disposal
  idiom (RAII destructor, finalizer, `close()`, generator cleanup) when
  iteration is abandoned early.

Generated wrappers do both for you; they surface `iter<T>` as the
target's native lazy iteration idiom and pull one element per consumer
step. See the [IDL reference](../reference/idl.md#iterator-types).

### Sync versus async returns

Everything above describes **synchronous** returns: the consumer
receives an owned value and owes the matching release call after
copying it.

**Async results invert the buffer rule.** The buffers passed to an
async completion callback (strings, bytes, arrays, boxed optional
scalars) are borrowed: they stay owned by the producer, are valid only
for the callback's duration, and are freed by the producer after the
callback returns. The consumer copies inside the callback and must not
free them. Owned-object results (records, rich enums, and interfaces,
including optionals of them) are the exception in both directions: the
callback receives ownership, adopts the pointer, and eventually calls
`_destroy`, exactly as a synchronous object return would. See
[Result ownership and threading](async.md#result-ownership-and-threading).

### Struct and interface lifecycle

Structs and interface objects are opaque on the consumer side. The
lifecycle is:

1. `*_create` (structs) or a declared constructor such as `*_open`
   (interfaces) allocates and returns a pointer; the consumer owns it.
2. `*_destroy` frees the object. Call exactly once.
3. `*_get_<field>` getters read struct fields, and interface methods
   take the receiver as their leading argument. Primitive getters
   (`i32`, `f64`, `bool`) return values directly. String/bytes getters
   return **new owned copies** that must be freed.

Functions that take an interface or `handle<T>` parameter always
**borrow** it: the producer must never free a receiver it is passed,
even for `close`-style functions. The only function that frees an
object is its `*_destroy` symbol. Generated wrappers call `*_destroy`
automatically (Swift `deinit`, Python `__del__`, Ruby
`FFI::AutoPointer`, ...), so a producer that frees a receiver inside
an ordinary function causes a double-free as soon as the wrapper is
garbage collected.

```c
weaveffi_error err = {0, NULL};

weaveffi_contacts_Contact* contact = weaveffi_contacts_Contact_create(
    1, "Alice", "Smith", "alice@example.com",
    weaveffi_contacts_ContactType_Personal,
    &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

int64_t id = weaveffi_contacts_Contact_get_id(contact);
const char* name = weaveffi_contacts_Contact_get_first_name(contact);
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

    public var first_name: String {
        let raw = weaveffi_contacts_Contact_get_first_name(ptr)
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

Generated wrappers clear the slot for you. On a `throws: true`
function they convert non-zero codes into the module's typed domain
error (`throw`, `raise`, `(T, error)`); on a non-throwing function a
non-zero code only ever reports a producer bug, so the wrapper panics
or traps instead. See the [Error Handling Guide](errors.md).

`weaveffi_error_clear` is idempotent: it frees the message and nulls
the pointer, so clearing an already-cleared slot is safe. That matters
for async completion callbacks, where the error struct is borrowed
from the producer (which releases the message itself after the
callback returns); a consumer that clears it anyway causes no
double-free.

### Thread safety

Generated FFI functions are expected to be called from a **single
thread** unless the module's documentation says otherwise. Concurrent
calls from multiple threads can cause data races and undefined
behaviour. Synchronise externally, for example with a mutex or a
serial dispatch queue:

```swift
let queue = DispatchQueue(label: "com.app.weaveffi")
queue.sync {
    let result = Calculator.add(a: 1, b: 2)
}
```

## Reference

| Resource           | Allocator | Free function              | Notes                                |
|--------------------|-----------|----------------------------|--------------------------------------|
| Returned string    | Rust      | `weaveffi_free_string`     | Every `const char*` return           |
| Returned bytes     | Rust      | `weaveffi_free_bytes`      | Pass both pointer and length         |
| Returned list      | Rust      | element plan, then `weaveffi_free_bytes` | Free each element first, then the buffer (`len * sizeof(T)`) |
| Returned map       | Rust      | element plans, then `weaveffi_free_bytes` twice | Keys and values first, then both parallel arrays |
| Boxed optional scalar | Rust   | `weaveffi_free_bytes`      | `sizeof(T)`; null means none, nothing to free |
| Struct instance    | Rust      | `*_destroy`                | Call exactly once                    |
| Interface instance | Rust      | `*_destroy`                | Call exactly once; methods borrow    |
| String from getter | Rust      | `weaveffi_free_string`     | Getter returns an owned copy         |
| Iterator handle    | Rust      | the iterator's `_destroy`  | Exactly once: on exhaustion or abandonment |
| Iterator element   | Rust      | element plan               | Each `_next` yields a consumer-owned element |
| Async result buffer | Rust     | none (borrowed)            | Producer frees after the callback returns; copy inside it |
| Async object result | Rust     | `*_destroy`                | Callback adopts ownership            |
| Error message      | Rust      | `weaveffi_error_clear`     | Clears code and frees message; idempotent |

## Pitfalls

- **Use-after-free**: reading a string after freeing it, or accessing
  a struct after `_destroy`. Once the consumer frees something, the
  pointer is invalid.
- **Double-free**: freeing the same pointer twice (e.g. calling
  `weaveffi_free_string` twice or invoking `_destroy` after the wrapper
  has already done so).
- **Wrong length to `weaveffi_free_bytes`**: always free with the
  exact length the C ABI returned in `out_len`.
- **Forgetting to clear error structs**: `err.message` is
  Rust-allocated; failing to call `weaveffi_error_clear` after a
  non-zero code leaks that string.
- **Calling FFI from multiple threads without synchronisation**: the
  default contract is single-threaded; synchronise externally if you
  need parallelism.
- **Manually freeing pointers passed in as borrowed parameters**:
  borrowed inputs (`&str`, `&[u8]`, `const T*`) are owned by the
  caller and must not be passed to `weaveffi_free_*`.
- **Freeing only the buffer of a list of strings or objects**: a
  returned `[string]` or `[Contact]` owes one release per element
  *before* the buffer release; skipping the element pass leaks every
  entry.
- **Freeing an async result buffer**: buffers passed to a completion
  callback are producer-owned and freed by the producer after the
  callback returns. Copy inside the callback; freeing there
  double-frees.
- **Destroying an iterator handle twice**: destroy it once, on
  exhaustion or when abandoning iteration early. Generated wrappers
  null the handle so their disposal idiom cannot double-destroy;
  hand-written C consumers must do the same.
