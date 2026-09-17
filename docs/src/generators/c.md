# C

## Overview

The C target emits the canonical C header and a thin reference C file
that every other WeaveFFI target ultimately speaks to. All cross-language
bindings sit on top of these symbols, so the C output is also the easiest
way to inspect what the IDL compiles to. The header is a rendering of the
[C ABI contract](../reference/abi.md), revision 2: reference-counted
objects, consumer-implemented callback-interface vtables, value buffers,
async launchers, and iterators.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/c/weaveffi.h` | Public header: enums, object types, vtables, function prototypes, error/memory helpers, and the value-buffer convention comment |
| `generated/c/weaveffi.c` | Default `weaveffi_alloc`/`weaveffi_dealloc` implementations (used by the Wasm JS glue); producers that ship their own allocator can omit it |

## Type mapping

| IDL type     | C parameter type                        | C return type                      |
|--------------|-----------------------------------------|------------------------------------|
| `i32`        | `int32_t`                               | `int32_t`                          |
| `u32`        | `uint32_t`                              | `uint32_t`                         |
| `i64`        | `int64_t`                               | `int64_t`                          |
| `u64`        | `uint64_t`                              | `uint64_t`                         |
| `i8`         | `int8_t`                                | `int8_t`                           |
| `i16`        | `int16_t`                               | `int16_t`                          |
| `u8`         | `uint8_t`                               | `uint8_t`                          |
| `u16`        | `uint16_t`                              | `uint16_t`                         |
| `f32`        | `float`                                 | `float`                            |
| `f64`        | `double`                                | `double`                           |
| `bool`       | `bool`                                  | `bool`                             |
| `string`     | `const char*` (NUL-terminated UTF-8, borrowed) | `const char*` (owned; `weaveffi_free_string`) |
| `bytes`      | `const uint8_t* ptr, size_t len` (borrowed) | `const uint8_t*` + `size_t* out_len` (owned; `weaveffi_free_bytes`) |
| `Struct`     | `const uint8_t* {name}_ptr, size_t {name}_len` (value buffer, borrowed) | `const uint8_t*` + `size_t* out_len` (value buffer, owned) |
| `Interface`  | `const weaveffi_m_I*` (borrowed)        | `weaveffi_m_I*` (one strong reference, adopted) |
| `Interface?` | `const weaveffi_m_I*` (null is "none")  | `weaveffi_m_I*` (null is "none")   |
| `CallbackInterface` | `void* {name}_ctx, const weaveffi_m_CI_vtable* {name}_vtable` | not allowed |
| `Enum` (plain) | `weaveffi_m_E`                        | `weaveffi_m_E`                     |
| `Enum` (rich)  | value buffer, like `Struct`           | value buffer, like `Struct`        |
| `T?` (other) | value buffer                            | value buffer                       |
| `[T]`        | value buffer                            | value buffer                       |
| `{K:V}`      | value buffer                            | value buffer                       |
| `iter<T>`    | n/a                                     | opaque iterator pointer (see [Iterators](#iterators)) |

Every buffered type (structs, rich enums, optionals, lists, maps) is one
serialized `(ptr, len)` pair in the
[value-buffer format](../reference/value-buffers.md): borrowed when passed
in, producer-allocated and freed with `weaveffi_free_bytes` when returned.
An object inside a buffer is a `u64` token that carries one strong
reference (see [Objects inside buffers](#objects-inside-buffers)).

C ABI symbol naming follows a strict convention:

| Kind              | Pattern                                           | Example                                       |
|-------------------|---------------------------------------------------|-----------------------------------------------|
| Function          | `weaveffi_{module}_{function}`                    | `weaveffi_events_route_once`                  |
| Enum type         | `weaveffi_{module}_{Enum}`                        | `weaveffi_kv_EntryKind`                       |
| Enum variant      | `weaveffi_{module}_{Enum}_{Variant}`              | `weaveffi_kv_EntryKind_Volatile`              |
| Interface type    | `weaveffi_{module}_{Interface}`                   | `weaveffi_kv_Store`                           |
| Interface member  | `weaveffi_{module}_{Interface}_{member}`          | `weaveffi_kv_Store_open`                      |
| Interface clone   | `weaveffi_{module}_{Interface}_clone`             | `weaveffi_kv_Store_clone`                     |
| Interface destroy | `weaveffi_{module}_{Interface}_destroy`           | `weaveffi_kv_Store_destroy`                   |
| Callback vtable   | `weaveffi_{module}_{CallbackInterface}_vtable`    | `weaveffi_events_Subscriber_vtable`           |
| Error enum        | `weaveffi_{module}_{Domain}`                      | `weaveffi_kv_KvError`                         |
| Error constant    | `weaveffi_{module}_{Domain}_{Code}`               | `weaveffi_kv_KvError_KeyNotFound`             |
| Async callback    | `weaveffi_{module}_{function}_callback`           | `weaveffi_kv_Store_compact_callback`          |
| Async launcher    | `weaveffi_{module}_{function}_async`              | `weaveffi_kv_Store_compact_async`             |
| Iterator type     | `weaveffi_{module}_{Function}Iterator`            | `weaveffi_kv_Store_ListKeysIterator`          |
| Iterator next     | `weaveffi_{module}_{Function}Iterator_next`       | `weaveffi_kv_Store_ListKeysIterator_next`     |
| Iterator destroy  | `weaveffi_{module}_{Function}Iterator_destroy`    | `weaveffi_kv_Store_ListKeysIterator_destroy`  |

`{Function}` is the function name converted to PascalCase (`list_keys`
becomes `ListKeys`). An iterator returned by an interface method nests
under the interface (`weaveffi_kv_Store_ListKeysIterator`), and so do
async launchers (`weaveffi_kv_Store_compact_async`). Nested modules join
their path with underscores (`weaveffi_kv_stats_get_stats`).

When the IDL sets `c_prefix`, every symbol, including the runtime helpers,
is rewritten with the new prefix.

## Example IDL and generated code

```yaml
version: "0.9.0"
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
          - { name: first_name, type: string }
          - { name: last_name, type: string }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: "i32?" }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32
```

The header opens with an include guard, standard headers, an `extern "C"`
block, the ABI revision, and the shared error/memory helpers (taken from the
`kvstore` sample's output):

```c
/* The WeaveFFI C ABI revision this header was generated against. The
   producer exports weaveffi_abi_version() so a consumer can refuse to load
   a library built for a different revision instead of misreading its
   error struct or value buffers. */
#define WEAVEFFI_ABI_VERSION 2u
WEAVEFFI_API uint32_t weaveffi_abi_version(void);

/* Error slot written by every fallible call. `payload_ptr`/`payload_len`
   hold the matched error code's fields serialized in the WeaveFFI value
   buffer format (null when the code declares no fields); both the message
   and the payload are released by weaveffi_error_clear. Positive codes are
   the module's declared error codes; negative codes are runtime traps:
   -1 generic, -2 producer panic, -3 marshalling failure, -4 a callback
   interface implementation raised. */
typedef struct weaveffi_error {
    int32_t code;
    const char* message;
    const uint8_t* payload_ptr;
    size_t payload_len;
} weaveffi_error;

/* Fill `err` with `code` and a producer-owned copy of `message`. Callback
   interface trampolines call this to report a failure in the consumer's
   implementation (code -4) without allocating with a foreign allocator. */
WEAVEFFI_API void weaveffi_error_set(weaveffi_error* err, int32_t code, const char* message);
WEAVEFFI_API void weaveffi_error_clear(weaveffi_error* err);

/* Async completion callbacks receive a heap-boxed error the consumer
   owns; weaveffi_error_free releases the message, the payload, and the
   box itself. Passing NULL is a safe no-op. */
WEAVEFFI_API void weaveffi_error_free(weaveffi_error* err);
WEAVEFFI_API void weaveffi_free_string(const char* ptr);
WEAVEFFI_API void weaveffi_free_bytes(uint8_t* ptr, size_t len);
```

A comment block near the top of the header states the value-buffer
convention: a buffered parameter named `v` expands to a borrowed
`const uint8_t* v_ptr, size_t v_len`, and a buffered return is a
producer-allocated buffer returned as `const uint8_t*` with a trailing
`size_t* out_len`, decoded by the caller and released with
`weaveffi_free_bytes`.

In the real output each prototype is prefixed with a `WEAVEFFI_API`
visibility macro (and deprecated functions with `WEAVEFFI_DEPRECATED`),
omitted from the shorter snippets on this page. See
[Symbol visibility](#symbol-visibility) for what it does and when you need
it.

Structs generate no C symbols of their own: a struct crosses the ABI as a
serialized value buffer, so a function that takes or returns one simply
carries the buffer slots. The consumer packs and unpacks the bytes per the
[value-buffer encoding](../reference/value-buffers.md):

```c
/* create_contact returns a Contact: a buffered return. */
const uint8_t* weaveffi_contacts_create_contact(
    const char* first_name,
    const char* last_name,
    size_t* out_len,
    weaveffi_error* out_err);
```

Enums turn into typed `enum` declarations with prefixed variants:

```c
typedef enum {
    weaveffi_contacts_ContactType_Personal = 0,
    weaveffi_contacts_ContactType_Work = 1,
    weaveffi_contacts_ContactType_Other = 2
} weaveffi_contacts_ContactType;
```

Optionals and lists are buffered too, so they use the same two-slot shape
(an optional encodes a flag byte, a list a `u32` count, then the elements):

```c
/* find_contact takes an i32? and returns a Contact?: both buffered. */
const uint8_t* weaveffi_contacts_find_contact(
    const uint8_t* id_ptr, size_t id_len,
    size_t* out_len,
    weaveffi_error* out_err);

/* list_contacts returns [Contact]: one buffer holding every record. */
const uint8_t* weaveffi_contacts_list_contacts(
    size_t* out_len,
    weaveffi_error* out_err);
```

Every synchronous function takes a trailing `weaveffi_error* out_err`. On
failure `out_err->code` is non-zero and `out_err->message` points at a
producer-allocated string the consumer must clear:

```c
weaveffi_error err = {0};
int32_t total = weaveffi_contacts_count_contacts(&err);
if (err.code != 0) {
    fprintf(stderr, "Error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
    return 1;
}
```

## Objects (interfaces)

An `interfaces:` entry lowers to a forward-declared opaque struct, one
prototype per member, and a `_clone`/`_destroy` pair. Constructors are
statics returning a pointer, methods take a leading `const {tag}* self`
argument before their declared parameters, and statics take no `self`. From
the `kvstore` sample's `Store`:

```c
typedef struct weaveffi_kv_Store weaveffi_kv_Store;

/* Constructor: returns one strong reference the caller adopts. */
WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_open(const char* path, weaveffi_error* out_err);

/* Static: no self slot. */
WEAVEFFI_API int64_t weaveffi_kv_Store_default_capacity(weaveffi_error* out_err);

/* Methods: a leading self slot, borrowed for the call. */
WEAVEFFI_API bool weaveffi_kv_Store_delete(const weaveffi_kv_Store* self, const char* key, weaveffi_error* out_err);
WEAVEFFI_API int64_t weaveffi_kv_Store_count(const weaveffi_kv_Store* self, weaveffi_error* out_err);

/** Returns a new strong reference to the same object (the pointer value is unchanged). Null is a no-op returning null. */
WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_clone(const weaveffi_kv_Store* self);
/** Releases one strong reference; the object is dropped when the last reference is released. Null is a no-op. */
WEAVEFFI_API void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
```

Objects are reference counted by the producer, so the C consumer deals in
strong references rather than owned allocations:

- Every pointer a constructor, function, iterator step, or async callback
  hands you is one strong reference. Release it with `_destroy` exactly
  once; the object is dropped when the last reference anywhere goes away.
- `_clone` mints another strong reference to the same object and returns
  the same pointer value. Use it whenever you want to store a pointer in
  two places, or hand one to a value buffer, and pair each clone with its
  own `_destroy`.
- An interface parameter (`const weaveffi_kv_Store* self`, or `store` on
  `weaveffi_kv_stats_get_stats`) is borrowed for the duration of the call;
  the producer clones it if it needs to retain it.
- Both `_clone` and `_destroy` accept null as a no-op, and a null `self`
  on any method is rejected with `MARSHAL_ERROR_CODE` (`-3`) before the
  producer runs.

There is no wrapper type in C, so there's no use-after-close guard: a
pointer that has been passed to `_destroy` is dangling, and it's the
caller's job to null it. The `conformance/c/kvstore.c` consumer shows the
idiom:

```c
weaveffi_kv_Store* cloned = weaveffi_kv_Store_clone(store);
assert(cloned == store);
weaveffi_kv_Store_destroy(store);
assert(count(cloned) == 2 && "clone outlives the destroyed original");
store = cloned;
```

### Nullable objects

`Interface?` doesn't use a value buffer: it stays a nullable pointer in
both directions, where null means "none". From `Store.larger(other:
Store?) -> Store?`:

```c
WEAVEFFI_API weaveffi_kv_Store* weaveffi_kv_Store_larger(const weaveffi_kv_Store* self, const weaveffi_kv_Store* other, weaveffi_error* out_err);
```

A non-null return is one strong reference to release, even when the
producer returned one of its inputs back to you (`larger` returns either
`self` or `other`, with a fresh reference either way).

### Objects inside buffers

Inside a value buffer (a record field, a list element, an optional, a map
value, an async result) an object is a `u64` pointer token that carries one
strong reference; whoever decodes the buffer adopts it. That has two
practical consequences:

- **Reading.** Each token you decode is yours to `_destroy`. Decode a
  buffer exactly once; decoding it twice would adopt the same reference
  twice.
- **Writing.** Never write a pointer you still hold. Write the result of
  `_clone`, since the producer's reader takes that reference:

```c
// Encode a StoreInfo. Object fields are written as fresh strong references
// (`_clone`), since the buffer's reader adopts them.
static void write_store_info(wv_writer* w, const char* label,
                             weaveffi_kv_Store* store, weaveffi_kv_Store* mirror,
                             int64_t n) {
    wv_put_str(w, label);
    wv_put_obj(w, weaveffi_kv_Store_clone(store));
    wv_put_bool(w, mirror != NULL);
    if (mirror) wv_put_obj(w, weaveffi_kv_Store_clone(mirror));
    wv_put_i64(w, n);
}
```

The `kvstore` sample exercises every position: `describe` returns a
`StoreInfo` record whose `store` field is the receiver itself, `open_many`
returns `[Store]`, and `total_count` takes `[Store]` plus `StoreInfo?`.
Iterator elements and async results that are objects follow the same
rule as returns: one strong reference per element, adopted by the
consumer.

## Callback interfaces

A `callback_interfaces:` entry is a set of methods the consumer implements
and the producer calls. It lowers to a vtable `typedef struct` and no
exported symbols. From the `events` sample's `Subscriber`:

```c
typedef struct weaveffi_events_Subscriber_vtable {
    /** Decide how the bus should treat `topic` for this subscriber. */
    weaveffi_events_Delivery (*route)(void* ctx, const char* topic, weaveffi_error* out_err);
    /**
     * Receive an accepted message. Returns the subscriber's running count
     * of received messages.
     */
    int64_t (*on_message)(void* ctx, const uint8_t* message_ptr, size_t message_len, weaveffi_error* out_err);
    /**
     * Receive the bus itself (an object handed through a callback). The
     * consumer adopts the reference and may keep or drop it.
     */
    void (*on_attached)(void* ctx, weaveffi_events_EventBus* bus, weaveffi_error* out_err);
    void (*free)(void* ctx);
} weaveffi_events_Subscriber_vtable;

WEAVEFFI_API int64_t weaveffi_events_EventBus_subscribe(const weaveffi_events_EventBus* self, void* subscriber_ctx, const weaveffi_events_Subscriber_vtable* subscriber_vtable, weaveffi_error* out_err);
WEAVEFFI_API weaveffi_events_Delivery weaveffi_events_route_once(void* subscriber_ctx, const weaveffi_events_Subscriber_vtable* subscriber_vtable, const char* topic, weaveffi_error* out_err);
```

A callback-interface parameter named `subscriber` lowers to two slots,
`void* subscriber_ctx` and `const {tag}_vtable* subscriber_vtable`. The
consumer supplies a heap context per implementation and one static vtable
per callback interface:

```c
typedef struct {
    const char* fail_topic;
    int64_t received;
    weaveffi_events_EventBus* bus;  // kept reference (when keep_bus)
    // ...
} sub_ctx;

static weaveffi_events_Delivery sub_route(void* ctx, const char* topic,
                                          weaveffi_error* out_err) {
    sub_ctx* s = (sub_ctx*)ctx;
    if (s->fail_topic && strcmp(topic, s->fail_topic) == 0) {
        weaveffi_error_set(out_err, -4, "subscriber rejected topic");
        return weaveffi_events_Delivery_Skip;
    }
    return weaveffi_events_Delivery_Accept;
}

// The bus arrives as one strong reference the consumer adopts: it is usable
// right here, and it is ours to keep or release.
static void sub_on_attached(void* ctx, weaveffi_events_EventBus* bus,
                            weaveffi_error* out_err) {
    (void)out_err;
    sub_ctx* s = (sub_ctx*)ctx;
    if (s->keep_bus) {
        s->bus = bus;
    } else {
        weaveffi_events_EventBus_destroy(bus);
    }
}

static void sub_free(void* ctx) {
    sub_ctx* s = (sub_ctx*)ctx;
    weaveffi_events_EventBus_destroy(s->bus);  // null is a no-op
    free(s);
}

static const weaveffi_events_Subscriber_vtable SUB_VTABLE = {
    sub_route,
    sub_on_message,
    sub_on_attached,
    sub_free,
};

sub_ctx* a = calloc(1, sizeof *a);
weaveffi_events_EventBus_subscribe(bus, a, &SUB_VTABLE, &err);
```

The contract, in full:

- **Lifetime.** The producer may call any method any number of times, from
  any thread, until it calls `free(ctx)` exactly once; after that it never
  touches `ctx` again. The producer holds the pair behind its own reference
  count, so `free` runs when its last clone drops: for `route_once`, before
  the call returns; for `subscribe`, when the bus is cleared or destroyed.
- **Argument ownership.** Strings, bytes, and value buffers passed to a
  method (`topic`, `message_ptr`/`message_len`) are borrowed for the call;
  copy what you need. An object passed to a method (`bus` above) is one
  strong reference the implementation owns: keep it or `_destroy` it. The
  vtable slot is `weaveffi_events_EventBus*`, not `const`, to make that
  visible.
- **Returns.** Methods return `void` or a Direct-family value (a scalar,
  `bool`, or a C-style enum), so nothing is allocated on the way back.
- **Failure.** To report that the implementation failed, call
  `weaveffi_error_set(out_err, -4, "message")` and return any value. The
  producer copies the message, aborts the current call, and the original
  caller observes `FOREIGN_ERROR_CODE` (`-4`) with your text in
  `out_err->message`, exactly as it would observe a producer panic. Never
  write `out_err->message` yourself; the producer frees it with its own
  allocator.
- **Reentrancy.** A method may call back into the producer (the
  `on_attached` above calls `subscriber_count` on the bus it was handed).
  The samples are written to never hold a lock across a callback; treat
  your own state the same way, since the method runs on whatever thread
  the producer is on (an `_async` method may call from a worker thread).

## Error codes

Every synchronous symbol writes its trailing `out_err`; async and
iterator paths deliver errors as described below. Positive codes are the
module's declared domain codes. The negative range is reserved:

| Code | Meaning | Where it comes from |
|------|---------|---------------------|
| `-1` | `GENERIC_ERROR_CODE` | an untyped producer error (`Result<T, String>`) |
| `-2` | `PANIC_ERROR_CODE` | the producer panicked, synchronously or inside a spawned async future; `message` carries the panic text |
| `-3` | `MARSHAL_ERROR_CODE` | an argument could not be lifted: a null `string` or `self` pointer, non-UTF-8 text, an out-of-range enum discriminant, a malformed value buffer |
| `-4` | `FOREIGN_ERROR_CODE` | one of your callback-interface methods called `weaveffi_error_set`; `message` carries your text |

C is the raw ABI, so throwing and non-throwing callables look identical:
every prototype carries `out_err`, and the consumer checks `err.code` after
each call. What a module's error domain adds is a typed enum naming the
positive codes its `throws: true` callables can report, so consumers match
on names instead of magic numbers. From the `kvstore` sample:

```c
/** Error codes reported by throwing functions in the `kv` module tree. */
typedef enum {
    /** key not found */
    weaveffi_kv_KvError_KeyNotFound = 1001,
    /** entry expired */
    weaveffi_kv_KvError_Expired = 1002,
    /** store has reached capacity */
    weaveffi_kv_KvError_StoreFull = 1003,
    /** I/O failure */
    weaveffi_kv_KvError_IoError = 1004
} weaveffi_kv_KvError;
```

A callable declared with `throws: true` can set any of these codes; a
callable without `throws` can only fail with the reserved negative codes.
The higher-level targets trap on a negative code from a non-throwing
function; in C the check is yours. See the
[Error Handling guide](../guides/errors.md) for the full picture.

## 64-bit integers and floats

`i64`/`u64` cross as `int64_t`/`uint64_t` by value, so the full range is
available with no conversion; the `codec` sample's `roundtrip_u64` returns
`UINT64_MAX` unchanged. `f32`/`f64` cross as `float`/`double`, and NaN,
the infinities, and negative zero survive the trip bit for bit; inside a
value buffer they are written as their IEEE 754 little-endian bytes.

## Symbol visibility

Every function prototype is tagged with a `WEAVEFFI_API` macro that the
header defines near the top:

```c
#ifndef WEAVEFFI_API
#  if defined(_WIN32) || defined(__CYGWIN__)
#    ifdef WEAVEFFI_BUILD
#      define WEAVEFFI_API __declspec(dllexport)
#    else
#      define WEAVEFFI_API __declspec(dllimport)
#    endif
#  elif defined(__EMSCRIPTEN__)
#    define WEAVEFFI_API __attribute__((used, visibility("default")))
#  elif defined(__GNUC__) && (__GNUC__ >= 4)
#    define WEAVEFFI_API __attribute__((visibility("default")))
#  else
#    define WEAVEFFI_API
#  endif
#endif
```

This covers the two ways the header is used:

- **Consuming** a prebuilt library (the common case) needs nothing extra.
  On Windows the prototypes resolve to `__declspec(dllimport)`; everywhere
  else the macro is harmless.
- **Implementing** the header (a C, C++, or Zig backend that supplies the
  symbols instead of calling them) relies on the macro to stay exportable.
  Under hidden default visibility (`-fvisibility=hidden`, the release-build
  norm and the MSVC default) an untagged definition is local and ships no
  usable symbol. On GCC and Clang the macro applies `visibility("default")`,
  so your definitions export with no extra flags.

When you implement the header on Windows, compile your library with
`WEAVEFFI_BUILD` defined so the macro switches to `__declspec(dllexport)`:

```sh
cc -DWEAVEFFI_BUILD -shared mylib.c -o mylib.dll
```

Deprecated functions carry a companion `WEAVEFFI_DEPRECATED("...")` macro
that expands to `__declspec(deprecated(...))` on MSVC and
`__attribute__((deprecated(...)))` on GCC and Clang.

When the IDL sets `c_prefix`, both macros follow it: a `c_prefix` of `acme`
yields `ACME_API`, `ACME_BUILD`, and `ACME_DEPRECATED`, so two
WeaveFFI-generated libraries can coexist in one translation unit without
colliding.

## Rich (algebraic) enums

An enum whose variants declare `fields` is a *rich* (algebraic) enum, a sum
type with associated data. Unlike a plain C-style enum (a bare `int32_t`
discriminant), a rich enum crosses the ABI as a serialized value buffer: an
`i32` tag (the variant's declared discriminant) followed by the active
variant's fields in declaration order. No per-enum C symbols are generated;
the consumer packs and unpacks the bytes. From the `codec` sample (`Shape`
= `Empty | Circle{radius} | Rect{width,height} | Labeled{label,count} |
Nested{inner,note}`), a function taking and returning a `Shape` looks like
any other buffered call:

```c
/** Return the argument unchanged. */
WEAVEFFI_API const uint8_t* weaveffi_codec_roundtrip_shape(const uint8_t* value_ptr, size_t value_len, size_t* out_len, weaveffi_error* out_err);
```

To build a `Circle{radius: 2.0}`, encode the tag and the payload
little-endian per the
[value-buffer encoding](../reference/value-buffers.md); to read a result,
decode the leading `i32` tag and then the matching variant's fields:

```c
weaveffi_error err = {0};

/* Encode Circle (tag 1) with radius 2.0. */
uint8_t shape[12];
int32_t tag = 1;
double radius = 2.0;
memcpy(shape, &tag, 4);
memcpy(shape + 4, &radius, 8);

size_t out_len = 0;
const uint8_t* back = weaveffi_codec_roundtrip_shape(shape, sizeof shape, &out_len, &err);
/* decode the returned tag and fields from `back` ... */
weaveffi_free_bytes((uint8_t*)back, out_len);
```

The consumer owns every buffer a function returns; release each one with
`weaveffi_free_bytes`. Buffers passed in stay owned by the caller.

## Build instructions

The runnable consumers use the sample crates and their conformance
programs; `conformance/c/wvbuf.h` is a small header-only value-buffer
writer and reader you can copy into your own project.

macOS:

```bash
cargo build -p events
weaveffi generate samples/events/src/lib.rs -o generated

cc -I generated/c -I conformance/c conformance/c/events.c -L target/debug -levents -o c_events
DYLD_LIBRARY_PATH=target/debug ./c_events
```

Linux:

```bash
cargo build -p events
weaveffi generate samples/events/src/lib.rs -o generated

cc -I generated/c -I conformance/c conformance/c/events.c -L target/debug -levents -o c_events
LD_LIBRARY_PATH=target/debug ./c_events
```

Windows:

```powershell
cargo build -p events
weaveffi generate samples\events\src\lib.rs -o generated
cl /I generated\c /I conformance\c conformance\c\events.c /link events.lib
.\events.exe
```

See `conformance/c/` for end-to-end consumers of every sample.

## Packaging

`weaveffi package --target c` emits the header under `c/include/`, one
prebuilt library per platform under `c/lib/<platform>/`, and a
`CMakeLists.txt` that picks the library matching the host and exposes it as
an imported target; `add_subdirectory` and link. Every library you pass
with `--binary` is copied, but the generated `CMakeLists.txt` only knows how
to select the desktop platforms (`darwin-arm64`, `darwin-x64`, `linux-x64`,
`linux-arm64`, `windows-x64`); Android and `wasm32` slices, if bundled, need
your own build logic to pick them up. See
[Packaging and Distribution](../guides/packaging.md).

## Memory and ownership

Rust always owns memory it allocates. Strings and byte buffers returned
across the boundary must be freed by the consumer with the matching
helper:

```c
const char* text = weaveffi_codec_roundtrip_string("héllo", &err);
printf("%s\n", text);
weaveffi_free_string(text);

size_t len;
const uint8_t* data = weaveffi_codec_roundtrip_bytes(in, in_len, &len, &err);
weaveffi_free_bytes((uint8_t*)data, len);
```

Returned value buffers (structs, rich enums, optionals, lists, maps) follow
the bytes rule: decode, then free once with `weaveffi_free_bytes(ptr,
out_len)`, and `_destroy` every object token you adopted from it. For
top-level objects, release each strong reference with `_destroy`. Borrowed
parameters (`const T*` objects, `string`/`bytes` inputs, buffered `(ptr,
len)` pairs) remain owned by the caller for the duration of the call only.

## Async support

Async functions (`async: true`) get no synchronous prototype. Each one
emits a per-function callback typedef, `(void* context, weaveffi_error*
err, <result slots>)`, and a launcher with the `_async` suffix. From the
`events` sample's `EventBus.publish_later`:

```c
typedef void (*weaveffi_events_EventBus_publish_later_callback)(void* context, weaveffi_error* err, int64_t result);

/** Publish from a producer thread, resolving with the accepted count. */
WEAVEFFI_API void weaveffi_events_EventBus_publish_later_async(const weaveffi_events_EventBus* self, const char* topic, const char* text, weaveffi_events_EventBus_publish_later_callback callback, void* context);
```

The launcher returns immediately; the producer runs the future on its
configured spawner (`weaveffi::set_spawner`; the default drives each future
on a dedicated thread) and invokes the callback exactly once, from an
arbitrary producer thread, with either a result or a populated error. The
launcher clones `self` for the duration of the call, so destroying your
own reference while the call is pending is safe.

Ownership inside the callback follows the async contract: everything the
callback receives is owned by the consumer. Copy or decode result buffers
(strings, bytes, and the serialized value buffers of buffered results),
then release them with `weaveffi_free_string` or `weaveffi_free_bytes`. An
object result is one strong reference to `_destroy`. A non-null `err` is
heap-boxed: copy its code, message, and payload, then release it exactly
once with `weaveffi_error_free` (null is a safe no-op, so the
`conformance/c/events.c` consumer calls it unconditionally):

```c
static void on_publish_later(void* context, weaveffi_error* err, int64_t result) {
    g_later_err = err ? err->code : 0;
    weaveffi_error_free(err);
    g_later_result = result;
    atomic_store(&g_later_done, 1);
}
```

A panic inside the spawned future is caught (`weaveffi::abi::CatchUnwind`)
and reported through `err` with `PANIC_ERROR_CODE`, so the callback still
fires exactly once; a callback-interface failure inside the future arrives
as `FOREIGN_ERROR_CODE` the same way.

For `cancellable: true` functions the launcher gains a
`weaveffi_cancel_token*` slot before the callback, and the runtime
provides the token lifecycle. From the `kvstore` sample's `Store.compact`:

```c
typedef void (*weaveffi_kv_Store_compact_callback)(void* context, weaveffi_error* err, int64_t result);

WEAVEFFI_API void weaveffi_kv_Store_compact_async(const weaveffi_kv_Store* self, weaveffi_cancel_token* cancel_token, weaveffi_kv_Store_compact_callback callback, void* context);

WEAVEFFI_API weaveffi_cancel_token* weaveffi_cancel_token_create(void);
WEAVEFFI_API void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token);
WEAVEFFI_API bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token);
WEAVEFFI_API void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token);
```

See [Async functions](../guides/async.md) for the full pattern.

## Iterators

Functions returning `iter<T>` produce an opaque iterator pointer plus
`_next`/`_destroy` functions instead of a materialized list. From the
`events` sample (`EventBus.messages` returns `iter<string>`):

```c
typedef struct weaveffi_events_EventBus_MessagesIterator weaveffi_events_EventBus_MessagesIterator;

WEAVEFFI_API weaveffi_events_EventBus_MessagesIterator* weaveffi_events_EventBus_messages(const weaveffi_events_EventBus* self, weaveffi_error* out_err);
WEAVEFFI_API int32_t weaveffi_events_EventBus_MessagesIterator_next(weaveffi_events_EventBus_MessagesIterator* iter, const char** out_item, weaveffi_error* out_err);
WEAVEFFI_API void weaveffi_events_EventBus_MessagesIterator_destroy(weaveffi_events_EventBus_MessagesIterator* iter);
```

`_next` writes the next element into the out-param and returns `1`, or
returns `0` when exhausted (leaving `*out_item` untouched). Failures are
reported through `out_err`, so check it after the loop ends. Element
ownership follows the usual return rules; each `next` hands over an element
the consumer now owns, so here each `const char*` must be freed with
`weaveffi_free_string`. Call `_destroy` exactly once when done, even if
iteration stopped early:

```c
weaveffi_error err = {0};
weaveffi_events_EventBus_MessagesIterator* it = weaveffi_events_EventBus_messages(bus, &err);
const char* item = NULL;
while (weaveffi_events_EventBus_MessagesIterator_next(it, &item, &err) != 0) {
    printf("%s\n", item);
    weaveffi_free_string(item);
}
if (err.code != 0) { /* a failing step ended the loop */ }
weaveffi_events_EventBus_MessagesIterator_destroy(it);
```

An iterator over a buffered element type writes each element as a
producer-allocated value buffer through `const uint8_t** out_item` plus a
`size_t* out_len`; decode it, then free it with `weaveffi_free_bytes` per
element. An iterator over objects writes a `{tag}**` slot holding one
strong reference per element, which you `_destroy` when done.

The higher-level targets wrap exactly these three symbols in their native
lazy idioms; only the C surface exposes them raw.

## Known limitations

- C has no wrapper type, so nothing stops you from using a pointer after
  `_destroy` or decoding a buffer with object tokens twice. Both are
  undefined behavior; the other targets guard against them.
- There is no generated value-buffer codec for C. You write the encoder and
  decoder yourself (or start from `conformance/c/wvbuf.h`).
- Async completion runs on a producer thread with no marshalling back to
  the caller; you synchronize (the conformance consumers use C11 atomics).
- Callback-interface methods run on the producer's thread and the vtable
  entries must be thread-safe; nothing serializes them for you.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: make sure the linker sees the
  cdylib (`-L target/debug -l<your-crate>`). The header alone is not
  enough.
- **Crashes inside `weaveffi_free_string`**: the pointer wasn't
  producer-allocated. Only free pointers returned from a generated
  function, iterator step, or async callback.
- **`err.code == -3` on a method call**: the `self` pointer was null, a
  `string` parameter was null or not UTF-8, or a value buffer you encoded
  is malformed. Check the buffer layout against the
  [protocol](../reference/value-buffers.md).
- **`err.code == -4` from a function you didn't expect to fail**: one of
  your vtable methods reported a foreign error. The message is yours.
- **`weaveffi.c` looks nearly empty**: that file only carries the default
  `weaveffi_alloc`/`weaveffi_dealloc` implementations for Wasm producers.
  All declarations live in `weaveffi.h`.
