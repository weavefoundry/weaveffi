# C++

## Overview

The C++ target emits a header-only library `weaveffi.hpp` that wraps the
C ABI in idiomatic C++17. Structs and interfaces become RAII classes with
deleted copies and movable handles, error domains map to typed exception
hierarchies, async functions return `std::future`, and listeners accept
`std::function` callbacks. A `CMakeLists.txt` is included so the generated
directory can be dropped into any CMake build.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/cpp/weaveffi.hpp` | Header-only bindings: extern "C" declarations, RAII wrappers, enum classes, inline function wrappers |
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
| `handle`     | `void*`                              | `void*`                     |
| `StructName` | `StructName`                         | `const StructName&`         |
| `InterfaceName` | `InterfaceName` (RAII class)      | `const InterfaceName&`      |
| `EnumName` (plain) | `EnumName` (`enum class`)      | `EnumName`                  |
| `EnumName` (rich)  | `EnumName` (RAII class)        | `const EnumName&`           |
| `T?`         | `std::optional<T>`                   | `const std::optional<T>&`   |
| `[T]`        | `std::vector<T>`                     | `const std::vector<T>&`     |
| `{K: V}`     | `std::unordered_map<K, V>`           | `const std::unordered_map<K, V>&` |
| `iter<T>`    | generated lazy range class (return only; see [Iterators](#iterators)) | n/a |

## Example IDL → generated code

```yaml
version: "0.5.0"
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

Structs become RAII handle wrappers with deleted copy and noexcept move:

```cpp
class Contact {
    void* handle_;
public:
    explicit Contact(void* h) : handle_(h) {}
    ~Contact() {
        if (handle_) weaveffi_contacts_Contact_destroy(
            static_cast<weaveffi_contacts_Contact*>(handle_));
    }
    Contact(const Contact&) = delete;
    Contact& operator=(const Contact&) = delete;
    Contact(Contact&& o) noexcept : handle_(o.handle_) { o.handle_ = nullptr; }

    std::string name() const {
        const char* raw = weaveffi_contacts_Contact_get_name(
            static_cast<const weaveffi_contacts_Contact*>(handle_));
        std::string ret(raw);
        weaveffi_free_string(raw);
        return ret;
    }
};
```

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
    auto result = weaveffi_contacts_create_contact(
        name.c_str(),
        email.has_value() ? email.value().c_str() : nullptr,
        age, &err);
    detail::check(err);
    return Contact(result);
}

} // namespace contacts
} // namespace weaveffi
```

The module namespace replaces the old flat `contacts_create_contact`
spelling; call it as `weaveffi::contacts::create_contact(...)`. Nested IDL
modules nest namespaces the same way (`weaveffi::kv::stats::get_stats`).

## Typed errors

`WeaveFFIError` extends `std::runtime_error` and carries the raw `code()`.
A module's error domain generates a typed hierarchy: one class named after
the domain, plus one subclass per declared code, each named in PascalCase
with exactly one `Error` suffix. From the `contacts` sample's
`ContactsError` domain:

```cpp
namespace weaveffi {

class ContactsError : public WeaveFFIError {
public:
    ContactsError(int32_t code, const std::string& msg) : WeaveFFIError(code, msg) {}
};

/** name must not be empty */
class InvalidNameError : public ContactsError { /* ... */ };

/** contact not found */
class NotFoundError : public ContactsError { /* ... */ };

} // namespace weaveffi
```

A callable declared with `throws: true` routes its failure through a
per-domain checker (`detail::check_contacts`) that throws the most specific
subclass, so you can catch a single code, the domain, or the generic base:

```cpp
try {
    auto contact = book.get(42);
} catch (const weaveffi::NotFoundError& e) {
    std::cerr << "Not found: " << e.what() << '\n';
} catch (const weaveffi::ContactsError& e) {
    std::cerr << "Contacts error " << e.code() << ": " << e.what() << '\n';
}
```

A callable without `throws` has the same C++ signature (C++ has no checked
exceptions), but its failures can only be producer bugs (a panic or a
marshalling failure), which arrive as the generic `weaveffi::WeaveFFIError`
rather than a domain type. An unknown code on the typed path falls back to
the domain class itself (`ContactsError`).

## Interfaces

An `interfaces:` entry becomes a move-only RAII class following the same
ownership model as struct wrappers. Constructors become static factories,
methods are instance members, statics are static members, and the
destructor calls the implicit C `_destroy` symbol. From the `kvstore`
sample's `Store` (trimmed):

```cpp
/** An embedded key-value store owning its entries */
class Store {
    void* handle_;

public:
    ~Store() {
        if (handle_) weaveffi_kv_Store_destroy(static_cast<weaveffi_kv_Store*>(handle_));
    }
    Store(const Store&) = delete;
    Store(Store&& other) noexcept;

    /** Open (or create) a store backed by the given filesystem path */
    static Store open(const std::string& path) {
        weaveffi_error err{};
        auto result = weaveffi_kv_Store_open(path.c_str(), &err);
        detail::check_kv(err);       // throws: true -> typed KvError path
        return Store(result);
    }

    /** Remove the entry for the given key, returning true if it existed */
    bool delete_(const std::string& key) const;

    /** Return the number of live entries in the store */
    int64_t count() const;           // no throws: generic check only

    /** Stream every key, optionally filtered by a prefix */
    ListKeysIterator list_keys(const std::optional<std::string>& prefix) const;

    /** Reclaim space asynchronously; returns the number of bytes reclaimed */
    std::future<int64_t> compact(weaveffi_cancel_token* cancel_token = nullptr) const;

    /** The largest number of live entries one store will hold */
    static int64_t default_capacity();
};
```

Method names keep their snake_case IDL spelling; a name that collides with
a C++ keyword gains a trailing underscore (`delete` → `delete_`).
Deprecated members carry `[[deprecated("...")]]`. An interface parameter is
passed as `const Store&` (borrowed); an interface return wraps the owned
pointer in a new instance.

## Rich (algebraic) enums

An enum whose variants declare `fields` is a *rich* (algebraic) enum, a sum
type with associated data. Plain C-style enums stay `enum class`; a rich enum
instead becomes an opaque RAII wrapper class with the same ownership model as a
struct wrapper, plus a nested `Tag`, static factory methods, and per-variant
getters. From the `shapes` sample:

```cpp
namespace weaveffi {

class Shape {
    void* handle_;
public:
    enum class Tag : int32_t { Empty = 0, Circle = 1, Rectangle = 2, Labeled = 3 };
    Tag tag() const;

    static Shape Empty();
    static Shape Circle(double radius);
    static Shape Rectangle(float width, float height);
    static Shape Labeled(const std::string& label, uint8_t count);

    double circle_radius() const;
    float rectangle_width() const;
    float rectangle_height() const;
    std::string labeled_label() const;
    uint8_t labeled_count() const;

    ~Shape();                       // calls weaveffi_shapes_Shape_destroy
    Shape(const Shape&) = delete;   // move-only, like struct wrappers
    Shape(Shape&&) noexcept;
};

} // namespace weaveffi
```

Build a variant with its factory, switch on `tag()`, and read only the
matching getters. Free functions take and return the wrapper by `const&` /
by value:

```cpp
weaveffi::Shape shape = weaveffi::Shape::Circle(2.0);

if (shape.tag() == weaveffi::Shape::Tag::Circle) {
    std::cout << "radius = " << shape.circle_radius() << '\n';
}

std::cout << weaveffi::shapes_describe(shape) << '\n';
weaveffi::Shape bigger = weaveffi::shapes_scale(shape, 3.0);
```

Ownership follows the struct-wrapper rules: the destructor calls
`weaveffi_shapes_Shape_destroy`, copies are deleted, and moves transfer the
handle, no manual free required.

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
(`libweaveffi.dylib`, `libweaveffi.so`, or `weaveffi.dll`).

## Memory and ownership

- Struct and interface wrappers own a single `void*` handle. The
  destructor calls the C `_destroy` function. Copies are deleted; moves
  transfer ownership by nulling the source handle.
- Strings returned from getters are copied into `std::string` and the
  raw pointer is freed via `weaveffi_free_string` before returning.
- Optional fields use `std::optional<T>`; a `nullptr` from the C layer
  becomes `std::nullopt`. A returned optional scalar arrives boxed
  behind a pointer; the wrapper dereferences it and frees the box with
  `weaveffi_free_bytes`.
- `std::vector<T>` returns own their contents: the wrapper copies each
  element (freeing string elements individually with
  `weaveffi_free_string`), then releases the producer's buffer with
  `weaveffi_free_bytes`; map returns release both parallel key/value
  buffers the same way. List parameters borrow the underlying buffer
  for the duration of the call.

## Callbacks and listeners

Listeners surface as free functions in the module's namespace taking
`std::function`. The register wrapper boxes the callable in a
`std::shared_ptr`, hands the C ABI a capture-less trampoline plus the raw
pointer as `context`, and pins the box in a global registry so it stays
alive until unregister. From the `events` sample (trimmed):

```cpp
namespace detail {

inline std::mutex& wv_listener_mutex() {
    static std::mutex m;
    return m;
}

inline std::unordered_map<uint64_t, std::shared_ptr<void>>& wv_listener_registry() {
    static std::unordered_map<uint64_t, std::shared_ptr<void>> registry;
    return registry;
}

} // namespace detail

namespace events {

inline uint64_t register_message_listener(std::function<void(std::string)> callback) {
    auto fn = std::make_shared<std::function<void(std::string)>>(std::move(callback));
    uint64_t id = weaveffi_events_register_message_listener(
        [](const char* message, void* context) {
            auto& cb = *static_cast<std::function<void(std::string)>*>(context);
            cb(std::string(message ? message : ""));
        },
        fn.get());
    std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());
    detail::wv_listener_registry()[id] = fn;
    return id;
}

inline void unregister_message_listener(uint64_t id) {
    weaveffi_events_unregister_message_listener(id);
    std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());
    detail::wv_listener_registry().erase(id);
}

} // namespace events
```

- `register_*` returns the `uint64_t` subscription id from the C
  layer. The registry (`detail::wv_listener_registry()`, a
  `std::unordered_map<uint64_t, std::shared_ptr<void>>` guarded by
  `detail::wv_listener_mutex()`) maps that id to the boxed
  `std::function`, keeping it alive while events can still fire.
- `unregister_*` first unregisters at the C layer, then erases the
  registry entry, releasing the callable.
- The static trampoline converts the C arguments to C++ types
  (`const char*` → `std::string`) before invoking the stored function.
- The callback runs on the producer's thread, not the thread that
  registered it; capture and synchronize accordingly.

```cpp
uint64_t id = weaveffi::events::register_message_listener(
    [](std::string message) { std::cout << message << '\n'; });
weaveffi::events::send_message("hello");
weaveffi::events::unregister_message_listener(id);
```

## Async support

Async IDL functions return `std::future<T>`. The wrapper allocates a
heap-owned `std::promise`, hands the C ABI a callback that resolves
(or rejects) the promise, and returns the corresponding future:

```cpp
inline std::future<Contact> fetch_contact(int32_t id) {
    auto* promise_ptr = new std::promise<Contact>();
    auto future = promise_ptr->get_future();
    weaveffi_contacts_fetch_contact_async(id,
        [](void* context, weaveffi_error* err,
           weaveffi_contacts_Contact* result) {
            auto* p = static_cast<std::promise<Contact>*>(context);
            if (err && err->code != 0) {
                std::string msg(err->message ? err->message : "unknown error");
                p->set_exception(detail::make_error(err->code, msg));
            } else {
                p->set_value(Contact(result));
            }
            delete p;
        }, static_cast<void*>(promise_ptr));
    return future;
}
```

Use it with `.get()` (blocking) or compose with your event loop. The
completion lambda runs exactly once, on an arbitrary producer thread; it
completes (or rejects) the promise and then deletes it. Result buffers
passed to the callback (strings, bytes, arrays, and the error message)
are borrowed from the producer for the callback's duration, so the
lambda copies them into C++ values before returning and never frees
them. Owned-object results are the exception: the callback receives
ownership, so `Contact(result)` above adopts the pointer into a RAII
wrapper. An async callable with `throws: true` rejects with the
module's typed domain exception (`detail::make_kv_error` and friends);
one without `throws` rejects with the generic `WeaveFFIError` only when
the producer has a bug.

When the IDL marks the callable `cancellable: true`, the wrapper gains
a trailing `weaveffi_cancel_token*` parameter defaulting to `nullptr`.
From the `kvstore` sample's async method `Store.compact`:

```cpp
/** Reclaim space asynchronously; returns the number of bytes reclaimed */
std::future<int64_t> compact(weaveffi_cancel_token* cancel_token = nullptr) const;
```

```cpp
weaveffi_cancel_token* token = weaveffi_cancel_token_create();
auto fut = store.compact(token);
weaveffi_cancel_token_cancel(token);   // from any thread
// fut.get() throws (typed KvError) if the operation was cancelled
weaveffi_cancel_token_destroy(token);
```

C++ is one of only three targets (C, C++, Kotlin) that expose the
cancel token; see [Async functions](../guides/async.md).

## Iterators

`iter<T>` return values surface as a generated move-only RAII range
class with `begin()`/`end()`, so results stream in constant memory:
nothing is drained up front, and each iteration step pulls exactly one
element from the producer through `_next`. From the `events` sample
(`get_messages` returns `iter<string>`, trimmed):

```cpp
/**
 * A lazy, move-only range over the `std::string` elements produced by `get_messages()`.
 */
class GetMessagesIterator {
    weaveffi_events_GetMessagesIterator* handle_;

public:
    ~GetMessagesIterator() {
        if (handle_) weaveffi_events_GetMessagesIterator_destroy(handle_);
    }
    GetMessagesIterator(const GetMessagesIterator&) = delete;
    GetMessagesIterator(GetMessagesIterator&&) noexcept;

    /** Pulls the next element, or `std::nullopt` once exhausted. */
    std::optional<std::string> next() {
        if (!handle_) return std::nullopt;
        weaveffi_error err{};
        const char* item{};
        int32_t has_item = weaveffi_events_GetMessagesIterator_next(handle_, &item, &err);
        if (err.code != 0) {
            weaveffi_events_GetMessagesIterator_destroy(handle_);
            handle_ = nullptr;
            detail::check(err);
        }
        if (has_item == 0) {
            weaveffi_events_GetMessagesIterator_destroy(handle_);
            handle_ = nullptr;
            return std::nullopt;
        }
        std::string value(item);
        weaveffi_free_string(item);
        return value;
    }

    struct sentinel {};

    /** Single-pass input iterator; each increment pulls one element. */
    class iterator { /* input_iterator_tag; compares against sentinel */ };

    iterator begin() { return iterator(this); }
    sentinel end() const { return sentinel{}; }
};

inline GetMessagesIterator get_messages() {
    weaveffi_error err{};
    weaveffi_events_GetMessagesIterator* iter = weaveffi_events_get_messages(&err);
    detail::check(err);
    return GetMessagesIterator(iter);
}
```

The range is single-pass: `begin()` returns an input iterator that
compares against a sentinel, so a plain range-`for` works:

```cpp
for (const std::string& message : weaveffi::events::get_messages()) {
    std::cout << message << '\n';
}
```

Each pulled string is copied into `std::string` and its native
allocation freed with `weaveffi_free_string`; record elements are
adopted by RAII wrappers. The producer iterator is destroyed exactly
once: eagerly when `next()` reports exhaustion (or an error), or from
the range's destructor when iteration is abandoned early (the handle
is nulled, so a double destroy is impossible).

Errors from the launcher and from each `next` follow the function's
error strategy. A throwing function like the `kvstore` sample's
`Store::list_keys` checks both through `detail::check_kv`, so the step
that failed throws the typed `KvError` subclass (after releasing the
iterator); a non-throwing function like `get_messages` throws the
generic `WeaveFFIError` only for producer bugs.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: link against the Rust
  cdylib. The header alone is not enough.
- **Double-free crashes**: RAII wrappers delete copy operators on
  purpose. If you see double-frees, somewhere you have a manual copy or
  a raw `void*` shared between wrappers.
- **Exceptions not caught across DLL boundaries on MSVC**: build the
  consumer and the dynamically loaded library with the same
  `_HAS_EXCEPTIONS` setting and CRT.
- **`std::optional` is missing**: the header requires C++17. Add
  `target_compile_features(... cxx_std_17)` to your CMake target.
