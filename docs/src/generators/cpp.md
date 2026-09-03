# C++

## Overview

The C++ target emits a header-only library `weaveffi.hpp` that wraps the
C ABI in idiomatic C++17. Structs become plain value structs, rich enums
become `std::variant`-backed sum types, interfaces become copyable RAII
classes holding one strong reference to a reference-counted producer
object, callback interfaces become abstract classes you subclass and pass
as `std::shared_ptr`, error domains map to typed exception hierarchies,
async functions return `std::future`, and `iter<T>` returns a lazy range.
The header inlines the same `extern "C"` declarations the C target emits,
so it's self-contained. A `CMakeLists.txt` is included so the generated
directory can be dropped into any CMake build.

The generated surface follows ABI revision 2 (`WEAVEFFI_ABI_VERSION`).
Call `weaveffi::check_abi_version()` once at startup; it throws
`WeaveFFIError` if the loaded library was built for a different revision.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/cpp/weaveffi.hpp` | Header-only bindings: extern "C" declarations, RAII wrappers, abstract callback classes, enum classes, inline function wrappers |
| `generated/cpp/CMakeLists.txt` | INTERFACE library target (`weaveffi_cpp`) |
| `generated/cpp/README.md` | Build instructions |

## Type mapping

| IDL type     | C++ type                             | Passed as parameter         |
|--------------|--------------------------------------|-----------------------------|
| `i32`        | `int32_t`                            | `int32_t`                   |
| `u32`        | `uint32_t`                           | `uint32_t`                  |
| `i64`        | `int64_t`                            | `int64_t`                   |
| `u64`        | `uint64_t`                           | `uint64_t`                  |
| `i8`         | `int8_t`                             | `int8_t`                    |
| `i16`        | `int16_t`                            | `int16_t`                   |
| `u8`         | `uint8_t`                            | `uint8_t`                   |
| `u16`        | `uint16_t`                           | `uint16_t`                  |
| `f32`        | `float`                              | `float`                     |
| `f64`        | `double`                             | `double`                    |
| `bool`       | `bool`                               | `bool`                      |
| `string`     | `std::string`                        | `const std::string&`        |
| `bytes`      | `std::vector<uint8_t>`               | `const std::vector<uint8_t>&` |
| `StructName` | `StructName` (value struct)          | `const StructName&`         |
| `InterfaceName` | `InterfaceName` (RAII class)      | `const InterfaceName&` (borrowed) |
| `InterfaceName?` | `std::optional<InterfaceName>`  | `const std::optional<InterfaceName>&` |
| `CallbackName` | abstract class `CallbackName`      | `std::shared_ptr<CallbackName>` |
| `EnumName` (plain) | `EnumName` (`enum class`)      | `EnumName`                  |
| `EnumName` (rich)  | `EnumName` (`std::variant`-backed sum type) | `const EnumName&` |
| `T?`         | `std::optional<T>`                   | `const std::optional<T>&`   |
| `[T]`        | `std::vector<T>`                     | `const std::vector<T>&`     |
| `{K: V}`     | `std::unordered_map<K, V>`           | `const std::unordered_map<K, V>&` |
| `iter<T>`    | generated lazy range class (return only; see [Iterators](#iterators)) | n/a |

64-bit integers are native `int64_t`/`uint64_t`, and `float`/`double`
cross the ABI as IEEE values, so NaN, the infinities, and `-0.0` round-trip
bit-for-bit (the `codec` sample's `roundtrip_u64` and `roundtrip_f64`
exercise this).

## Example IDL → generated code

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
          - { name: contact_type, type: ContactType }

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32

      - name: fetch_contact
        async: true
        params:
          - { name: id, type: i32 }
        return: Contact
```

Enums become `enum class`:

```cpp
enum class ContactType : int32_t {
    Personal = 0,
    Work = 1,
    Other = 2
};
```

Structs become plain value structs with typed members:

```cpp
struct Contact {
    std::string name;
    std::optional<std::string> email;
    int32_t age;
    ContactType contact_type;
};
```

There are no C symbols behind a struct. A `Contact` crosses the ABI
serialized in the [value-buffer format](../reference/value-buffers.md) as a
single `(const uint8_t*, size_t)` pair; the header carries a small private
buffer reader and writer in the `detail` namespace plus one generated pack
and unpack routine per type.

Free functions live in a nested namespace per module inside the outer
`weaveffi` namespace (configurable via `namespace`), keeping their
snake_case IDL names with no module prefix, and throw on failure:

```cpp
namespace weaveffi {
namespace contacts {

inline Contact create_contact(
    const std::string& name,
    const std::optional<std::string>& email,
    int32_t age)
{
    weaveffi_error err{};
    // Optionals are buffered: pack the argument into a value buffer.
    std::vector<uint8_t> email_buf = /* generated pack routine */;
    size_t out_len = 0;
    const uint8_t* raw = weaveffi_contacts_create_contact(
        name.c_str(),
        email_buf.data(), email_buf.size(),
        age, &out_len, &err);
    detail::check(err);
    Contact ret = /* generated unpack routine over (raw, out_len) */;
    weaveffi_free_bytes(raw, out_len);
    return ret;
}

} // namespace contacts
} // namespace weaveffi
```

Call it as `weaveffi::contacts::create_contact(...)`. Nested IDL modules
nest namespaces the same way (`weaveffi::kv::stats::get_stats`).

## Objects (interfaces)

An `interfaces:` entry becomes a copyable RAII class that holds exactly one
strong reference to a reference-counted object owned by the producer.
Constructors become static factories (or a real C++ constructor when the
IDL constructor is named `new`, as `EventBus()` in the `events` sample),
methods are instance members, and statics are static members. From the
`kvstore` sample's `Store`:

```cpp
class Store {
    weaveffi_kv_Store* handle_;

public:
    /** Adopts one strong reference to a producer object. */
    explicit Store(weaveffi_kv_Store* h) : handle_(h) {}

    /** Releases this wrapper's reference; the object is dropped with its last one. */
    ~Store() {
        if (handle_) weaveffi_kv_Store_destroy(handle_);
    }

    /** Copies share the object: the copy takes a new strong reference. */
    Store(const Store& other) : handle_(weaveffi_kv_Store_clone(other.handle_)) {}

    Store(Store&& other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    /** The wrapped pointer, borrowed: this wrapper keeps its reference. */
    const weaveffi_kv_Store* handle() const { return handle_; }

    /** A new strong reference the caller owns (for example to write into a value buffer). */
    weaveffi_kv_Store* clone_handle() const { return weaveffi_kv_Store_clone(handle_); }

    static Store open(const std::string& path);
    bool delete_(const std::string& key) const;
    int64_t count() const;
    StoreListKeysIterator list_keys(const std::optional<std::string>& prefix) const;
    std::future<int64_t> compact(weaveffi_cancel_token* cancel_token = nullptr) const;
    static int64_t default_capacity();
};
```

- **Disposal is RAII.** The destructor calls `_destroy` once, releasing
  this wrapper's reference; the producer drops the object when the last
  reference (from any wrapper, any record field, or the producer's own
  retention) goes away. There is no public `destroy()` or `close()`.
- **Copies mint a new strong reference.** The copy constructor and copy
  assignment call `_clone`, so `Store copy = store;` yields a second wrapper
  over the *same* object (`copy.handle() == store.handle()`); a mutation
  through one is visible through the other, and each destructor releases
  its own reference. Moves transfer the pointer and leave the source empty.
- **Use after move.** A moved-from wrapper has `handle() == nullptr`. Its
  destructor is a no-op, but calling a method on it passes a null object to
  the producer, which rejects the call with `MARSHAL_ERROR_CODE` (-3) as a
  `WeaveFFIError`. Don't reuse a moved-from wrapper.
- `handle()` borrows the raw pointer for the duration of a call;
  `clone_handle()` returns a fresh strong reference you must eventually
  pass to `_destroy` (or adopt into another wrapper).

Method names keep their snake_case IDL spelling; a name that collides with
a C++ keyword gains a trailing underscore (`delete` → `delete_`).
Deprecated members carry `[[deprecated("...")]]`.

### Objects as parameters, returns, and inside values

A top-level object parameter is passed as `const Store&` and borrowed for
the call. A returned object is adopted into a new wrapper. `Store?` maps to
`std::optional<Store>`: a disengaged optional passes a null pointer, and a
null return becomes `std::nullopt`:

```cpp
inline std::optional<Store> Store::larger(const std::optional<Store>& other) const {
    weaveffi_error err{};
    auto result = weaveffi_kv_Store_larger(handle_, other.has_value() ? other->handle() : nullptr, &err);
    detail::check(err);
    if (!result) return std::nullopt;
    return Store(result);
}
```

Objects inside records, lists, map values, optionals, and rich-enum
payloads are held by value (`Store store; std::optional<Store> mirror;` in
`StoreInfo`). On the wire they're `u64` tokens: the generated pack routine
calls `clone_handle()` for each one, so the producer receives its own
reference and your wrapper stays valid, and the unpack routine adopts the
token into a wrapper:

```cpp
inline void write_StoreInfo(BufferWriter& w, const StoreInfo& v) {
    w.write_string(v.label);
    w.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>(v.store.clone_handle())));
    w.write_option_flag(v.mirror.has_value());
    if (v.mirror.has_value()) {
        w.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>((*v.mirror).clone_handle())));
    }
    w.write_i64(v.count);
}

inline StoreInfo read_StoreInfo(BufferReader& r) {
    std::string f_label = r.read_string();
    Store f_store = Store(reinterpret_cast<weaveffi_kv_Store*>(static_cast<uintptr_t>(r.read_u64())));
    // ...
}
```

`Store::open_many` returns `std::vector<Store>` (one adopted wrapper per
element), and `Store::total_count(const std::vector<Store>&, const std::optional<StoreInfo>&)`
clones every object it encodes. Copying a record that contains an object
copies the wrapper, which clones the reference.

## Typed errors

`WeaveFFIError` extends `std::runtime_error` and carries the raw `code()`.
A module's error domain generates a typed hierarchy: one class named after
the domain, plus one subclass per declared code, each named in PascalCase
with exactly one `Error` suffix. From the `kvstore` sample's `KvError`
domain:

```cpp
class KvError : public WeaveFFIError {
public:
    KvError(int32_t code, const std::string& msg) : WeaveFFIError(code, msg) {}
};

/** key not found */
class KeyNotFoundError : public KvError {
public:
    KeyNotFoundError(const std::string& msg) : KvError(1001, msg) {}
};
```

A callable declared with `throws: true` routes its failure through a
per-domain checker (`detail::check_kv`) that throws the most specific
subclass, so you can catch a single code, the domain, or the generic base:

```cpp
try {
    auto entry = store.get("missing");
} catch (const weaveffi::KeyNotFoundError& e) {
    std::cerr << "Not found: " << e.what() << '\n';
} catch (const weaveffi::KvError& e) {
    std::cerr << "kv error " << e.code() << ": " << e.what() << '\n';
}
```

An error code that declares payload `fields:` exposes them as typed members
on its subclass, decoded from the error's payload buffer before
`weaveffi_error_clear` releases it. An unknown positive code on the typed
path falls back to the domain class itself (`KvError`).

### Runtime error codes

Domain codes are validated positive-only, so a negative runtime code always
surfaces as the generic `WeaveFFIError`, never a typed domain exception.
That's also why no wrapper is `noexcept`: a callable without `throws` has
the same C++ signature and still checks `out_err` through `detail::check`.

The header doesn't name these codes; the names below are the ABI's (see
[ABI](../reference/abi.md)).

| Code | ABI name | When |
|------|----------|------|
| -1 | `GENERIC_ERROR_CODE` | The producer reported a failure with no domain code (also what `check_abi_version()` throws) |
| -2 | `PANIC_ERROR_CODE` | The Rust producer panicked inside an export or a spawned async future |
| -3 | `MARSHAL_ERROR_CODE` | A null object pointer or a malformed value buffer or string was rejected at the boundary |
| -4 | `FOREIGN_ERROR_CODE` | A callback-interface implementation threw |

Sync callables throw from the call; async callables reject the future so
`.get()` rethrows; iterator steps throw from `next()` (after releasing the
producer iterator).

## Callback interfaces

A `callback_interfaces:` entry becomes an abstract class with one pure
virtual method per IDL method. Subclass it and pass a
`std::shared_ptr<Iface>` to any function that accepts the interface. From
the `events` sample:

```cpp
class Subscriber {
public:
    virtual ~Subscriber() = default;

    virtual Delivery route(const std::string& topic) = 0;
    virtual int64_t on_message(const Message& message) = 0;
    virtual void on_attached(EventBus bus) = 0;
};
```

```cpp
class RecordingSubscriber : public Subscriber {
    std::optional<EventBus> kept_bus_;

public:
    Delivery route(const std::string& topic) override {
        return topic == "quiet" ? Delivery::Skip : Delivery::Accept;
    }
    int64_t on_message(const Message& message) override {
        std::cout << message.topic << ": " << message.text << '\n';
        return 1;
    }
    void on_attached(EventBus bus) override {
        kept_bus_ = std::move(bus);   // or let it drop to release the reference
    }
};

weaveffi::EventBus bus;
bus.subscribe(std::make_shared<RecordingSubscriber>());
```

Under the hood the wrapper boxes your `shared_ptr` on the heap as the
vtable `ctx`, hands the producer the one process-wide static vtable for the
interface, and the vtable's `free` deletes the box when the producer drops
its last reference (which runs your destructor):

```cpp
inline int64_t EventBus::subscribe(std::shared_ptr<Subscriber> subscriber) const {
    if (!subscriber) throw std::invalid_argument("subscriber: null callback interface");
    auto* subscriber_ctx = new std::shared_ptr<Subscriber>(std::move(subscriber));
    // ... weaveffi_events_EventBus_subscribe(handle_, subscriber_ctx, &detail::Subscriber_vtable(), &err)
}
```

- **Argument ownership.** Strings and buffered values (`const Message&`)
  are decoded into temporaries that live for the duration of the call;
  copy them if you need them afterward. An object argument
  (`EventBus bus`) is passed *by value* and is owned by your
  implementation: keep it (move it into a member) or let it go out of
  scope to release the reference.
- **Lifetime.** The producer may keep the implementation as long as it
  likes (`subscribe` retains it until `clear_subscribers`); a function that
  only uses it for the call (`route_once`) frees it before returning.
  Passing a null `shared_ptr` throws `std::invalid_argument` before
  anything crosses the ABI.
- **Exceptions.** Trampolines never let an exception unwind through the C
  frame. Anything derived from `std::exception` is reported with
  `weaveffi_error_set(out_err, -4, e.what())`; anything else gets a
  generic message. The producer aborts the call that triggered the
  callback, and the caller sees `WeaveFFIError` with `code() == -4`
  carrying your `what()`, for a `throws` and a non-`throws` callable alike,
  and through the future of an async callable. The implementation stays
  attached.
- **Threads.** Methods run on whichever thread the producer calls from: the
  calling thread for a synchronous call, a producer worker for an async
  one (`publish_later` in the sample). Synchronize shared state yourself;
  the wrapper doesn't marshal calls onto any particular thread.

## Rich (algebraic) enums

An enum whose variants declare `fields` is a *rich* (algebraic) enum, a sum
type with associated data. Plain C-style enums stay `enum class`; a rich enum
instead becomes a `std::variant`-backed sum type: one plain payload struct per
variant carrying that variant's fields, aggregated into a variant type named
after the enum. From the `shapes` sample, the surface follows this shape:

```cpp
namespace weaveffi {

struct ShapeEmpty {};
struct ShapeCircle { double radius; };
struct ShapeRectangle { float width; float height; };
struct ShapeLabeled { std::string label; uint8_t count; };

using Shape = std::variant<ShapeEmpty, ShapeCircle, ShapeRectangle, ShapeLabeled>;

} // namespace weaveffi
```

Values are plain data. Build the payload struct you need and inspect results
with `std::holds_alternative`, `std::get`, or `std::visit`:

```cpp
weaveffi::Shape shape = weaveffi::ShapeCircle{2.0};

if (auto* c = std::get_if<weaveffi::ShapeCircle>(&shape)) {
    std::cout << "radius = " << c->radius << '\n';
}

std::cout << weaveffi::shapes::describe(shape) << '\n';
weaveffi::Shape bigger = weaveffi::shapes::scale(shape, 3.0);
```

On the wire a rich enum is a value buffer holding an `i32` tag followed by
the active variant's fields; there are no per-variant C constructors, tag
readers, or destroy symbols. The wrappers pack and unpack the buffer, so no
manual free is required. A variant payload may hold an object; it follows
the token rules above.

## Build instructions

The generated `CMakeLists.txt` defines an INTERFACE library (the
project version mirrors `package.version` from the IDL):

```cmake
cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp VERSION 1.0.0)
add_library(weaveffi_cpp INTERFACE)
target_include_directories(weaveffi_cpp INTERFACE ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)
target_compile_features(weaveffi_cpp INTERFACE cxx_std_17)
```

Consume it from your project:

```cmake
add_subdirectory(path/to/generated/cpp)
add_executable(myapp main.cpp)
target_link_libraries(myapp weaveffi_cpp)
```

Then `#include "weaveffi.hpp"` and link against the Rust shared library
(`libweaveffi.dylib`, `libweaveffi.so`, or `weaveffi.dll`). The header
needs `-std=c++17` (or `cxx_std_17`); `std::optional`, `std::variant`, and
`std::future` come from the standard library, so no other dependency is
required.

## Packaging

`weaveffi package --target cpp` emits the header under `cpp/include/`,
one prebuilt library per desktop platform under `cpp/lib/<platform>/`, and
a `CMakeLists.txt` that selects the library matching the host and links it
into the `weaveffi_cpp` interface target. Only desktop slices are bundled
(`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `windows-x64`);
Android and `wasm32` binaries are skipped. See
[Packaging and Distribution](../guides/packaging.md).

## Memory and ownership

- Interface wrappers own one strong reference. The destructor releases it;
  copies clone it; moves transfer it. Nothing else to do.
- Structs, rich enums, optionals, lists, and maps are plain C++ values.
  They cross the ABI serialized in a single value buffer: parameters are
  packed by the wrapper and borrowed by the callee for the duration of
  the call, and returns are unpacked into the C++ value and the
  producer's buffer is released with `weaveffi_free_bytes`. Object fields
  inside those values are cloned on the way in and adopted on the way out.
- Returned strings are copied into `std::string` and the raw pointer is
  freed via `weaveffi_free_string` before returning.
- Callback-interface implementations are owned jointly by your
  `shared_ptr` and the producer's box; the producer's `free` releases its
  share.

## Async support

Async IDL functions return `std::future<T>`. The wrapper allocates a
heap-owned `std::promise`, hands the C ABI a callback that resolves
(or rejects) the promise, and returns the corresponding future. From the
`kvstore` sample's cancellable `Store::compact`:

```cpp
inline std::future<int64_t> Store::compact(weaveffi_cancel_token* cancel_token) const {
    auto* promise_ptr = new std::promise<int64_t>();
    auto future = promise_ptr->get_future();
    weaveffi_kv_Store_compact_async(handle_, cancel_token, [](void* context, weaveffi_error* err, int64_t result) {
        auto* p = static_cast<std::promise<int64_t>*>(context);
        try {
            if (err && err->code != 0) {
                std::string msg(err->message ? err->message : "unknown error");
                p->set_exception(detail::make_kv_error(err->code, msg, err->payload_ptr, err->payload_len));
            } else {
                p->set_value(result);
            }
        } catch (...) {
            p->set_exception(std::current_exception());
        }
        weaveffi_error_free(err);
        delete p;
    }, static_cast<void*>(promise_ptr));
    return future;
}
```

Use it with `.get()` (blocking) or compose with your event loop. The
completion lambda runs exactly once, on an arbitrary producer thread; it
settles the promise and then deletes it. Everything passed to the callback
is owned by the consumer: strings, bytes, and buffered results are copied
into C++ values and released with `weaveffi_free_string` or
`weaveffi_free_bytes`, a heap-boxed error is released with
`weaveffi_error_free`, and an object result is adopted into a wrapper
(`std::future<Store>`). An async callable with `throws: true` rejects with
the module's typed domain exception; one without `throws` rejects with the
generic `WeaveFFIError` for runtime codes (a panic in the future is -2, a
callback that threw is -4).

When the IDL marks the callable `cancellable: true`, the wrapper gains
a trailing `weaveffi_cancel_token*` parameter defaulting to `nullptr`:

```cpp
weaveffi_cancel_token* token = weaveffi_cancel_token_create();
auto fut = store.compact(token);
weaveffi_cancel_token_cancel(token);   // from any thread
// fut.get() throws (typed KvError) if the operation was cancelled
weaveffi_cancel_token_destroy(token);
```

C++ is one of the few targets (C, C++, Kotlin) that expose the cancel
token; see [Async functions](../guides/async.md).

## Iterators

`iter<T>` return values surface as a generated move-only RAII range
class with `begin()`/`end()`, so results stream in constant memory:
nothing is drained up front, and each iteration step pulls exactly one
element from the producer through `_next`. From the `kvstore` sample
(`Store::list_keys` returns `iter<string>`, trimmed):

```cpp
class StoreListKeysIterator {
    weaveffi_kv_Store_ListKeysIterator* handle_;

public:
    ~StoreListKeysIterator() {
        if (handle_) weaveffi_kv_Store_ListKeysIterator_destroy(handle_);
    }
    StoreListKeysIterator(const StoreListKeysIterator&) = delete;
    StoreListKeysIterator(StoreListKeysIterator&& other) noexcept;

    /** Pulls the next element, or `std::nullopt` once exhausted. */
    std::optional<std::string> next() {
        if (!handle_) return std::nullopt;
        weaveffi_error err{};
        const char* item{};
        int32_t has_item = weaveffi_kv_Store_ListKeysIterator_next(handle_, &item, &err);
        if (err.code != 0) {
            weaveffi_kv_Store_ListKeysIterator_destroy(handle_);
            handle_ = nullptr;
            detail::check_kv(err);
        }
        if (has_item == 0) {
            weaveffi_kv_Store_ListKeysIterator_destroy(handle_);
            handle_ = nullptr;
            return std::nullopt;
        }
        std::string value(item);
        weaveffi_free_string(item);
        return value;
    }

    struct sentinel {};
    class iterator { /* input_iterator_tag; compares against sentinel */ };
    iterator begin() { return iterator(this); }
    sentinel end() const { return sentinel{}; }
};
```

The range is single-pass: `begin()` returns an input iterator that
compares against a sentinel, so a plain range-`for` works:

```cpp
for (const std::string& key : store.list_keys(std::nullopt)) {
    std::cout << key << '\n';
}
```

Each pulled string is copied into `std::string` and its native allocation
freed with `weaveffi_free_string`; a buffered element arrives as a value
buffer that is unpacked and released with `weaveffi_free_bytes`; an object
element is adopted into a wrapper. The producer iterator is destroyed
exactly once: eagerly when `next()` reports exhaustion (or an error), or
from the range's destructor when iteration is abandoned early.

Errors from the launcher and from each `next` follow the function's error
strategy: `Store::list_keys` checks both through `detail::check_kv`, so a
failing step throws the typed `KvError` subclass after releasing the
iterator, while a non-throwing function throws the generic `WeaveFFIError`
only for runtime codes.

## Known limitations

- A callback interface must be passed as `std::shared_ptr`; there is no
  overload for a raw reference or a lambda. Wrap a lambda in a small
  subclass if you need one.
- Trampolines only capture `what()` from exceptions derived from
  `std::exception`; other thrown types are reported with a generic
  message.
- Exceptions thrown by callback methods that return `void` and belong to a
  non-`throws` callable still abort the call with -4; C++ has no separate
  trap path, so the caller simply sees `WeaveFFIError`.
- The lazy range is single-pass and move-only; copy the elements into a
  `std::vector` if you need random access or a second pass.
- `std::future` has no cancellation or continuation support of its own;
  cancellation goes through the raw `weaveffi_cancel_token*`.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: link against the Rust
  cdylib. The header alone is not enough.
- **`WeaveFFIError` with code -3 on a method call**: the wrapper's handle is
  null. You're calling through a moved-from wrapper or one constructed from
  a null pointer.
- **Double-free crashes**: every wrapper releases exactly one reference. A
  double free means a raw pointer from `handle()` was adopted by a second
  wrapper without `clone_handle()`, or a `clone_handle()` result was
  destroyed twice.
- **Exceptions not caught across DLL boundaries on MSVC**: build the
  consumer and the dynamically loaded library with the same
  `_HAS_EXCEPTIONS` setting and CRT.
- **`std::optional` is missing**: the header requires C++17. Add
  `target_compile_features(... cxx_std_17)` to your CMake target.
