# C++

The C++ generator emits a single header-only library `weaveffi.hpp` that
wraps the C ABI with idiomatic C++ types. Structs use RAII classes with
move semantics, errors become exceptions, and async functions return
`std::future`. A `CMakeLists.txt` is included for easy integration.

## Generated artifacts

- `generated/cpp/weaveffi.hpp` — header-only C++ bindings (extern "C" declarations, RAII wrapper classes, enum classes, inline function wrappers)
- `generated/cpp/CMakeLists.txt` — INTERFACE library target for CMake
- `generated/cpp/README.md` — build instructions

## RAII approach

All structs are wrapped as C++ classes that own a `void*` handle to
Rust-allocated data. The destructor calls the C ABI `_destroy` function,
so resources are freed automatically when the object goes out of scope —
no manual cleanup required.

Copy construction and copy assignment are **deleted** to prevent
double-free bugs. Move construction and move assignment are supported,
transferring ownership by nulling out the source handle.

This design means you can use standard C++ patterns like returning
structs from functions, storing them in containers via `std::move`, and
relying on stack unwinding for cleanup during exceptions.

## Generated code examples

Given this IDL definition:

```yaml
version: "0.1.0"
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

### Enums

Enums map to C++ `enum class` backed by `int32_t`:

```cpp
enum class ContactType : int32_t {
    Personal = 0,
    Work = 1,
    Other = 2
};
```

### Structs (RAII wrapper classes)

Structs are wrapped as C++ classes holding a `void*` handle to the
Rust-allocated data. The destructor calls the C ABI destroy function.
Field access is through getter methods that call the C ABI getters:

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

    Contact(Contact&& other) noexcept : handle_(other.handle_) {
        other.handle_ = nullptr;
    }

    Contact& operator=(Contact&& other) noexcept {
        if (this != &other) {
            if (handle_) weaveffi_contacts_Contact_destroy(
                static_cast<weaveffi_contacts_Contact*>(handle_));
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    void* handle() const { return handle_; }

    std::string name() const {
        const char* raw = weaveffi_contacts_Contact_get_name(
            static_cast<const weaveffi_contacts_Contact*>(handle_));
        std::string ret(raw);
        weaveffi_free_string(raw);
        return ret;
    }

    std::optional<std::string> email() const {
        auto* raw = weaveffi_contacts_Contact_get_email(
            static_cast<const weaveffi_contacts_Contact*>(handle_));
        if (!raw) return std::nullopt;
        std::string ret(raw);
        weaveffi_free_string(raw);
        return ret;
    }

    int32_t age() const {
        return weaveffi_contacts_Contact_get_age(
            static_cast<const weaveffi_contacts_Contact*>(handle_));
    }

    ContactType contact_type() const {
        return static_cast<ContactType>(
            weaveffi_contacts_Contact_get_contact_type(
                static_cast<const weaveffi_contacts_Contact*>(handle_)));
    }
};
```

### Functions

Module functions are generated as `inline` free functions in the
`weaveffi` namespace. Every function checks the error struct after calling
the C ABI and throws `WeaveFFIError` on failure:

```cpp
namespace weaveffi {

inline Contact contacts_create_contact(
    const std::string& name,
    const std::optional<std::string>& email,
    int32_t age)
{
    weaveffi_error err{};
    auto result = weaveffi_contacts_create_contact(
        name.c_str(),
        email.has_value() ? email.value().c_str() : nullptr,
        age, &err);
    if (err.code != 0) {
        std::string msg(err.message ? err.message : "unknown error");
        int32_t code = err.code;
        weaveffi_error_clear(&err);
        throw WeaveFFIError(code, msg);
    }
    return Contact(result);
}

inline std::optional<Contact> contacts_find_contact(int32_t id) {
    weaveffi_error err{};
    auto result = weaveffi_contacts_find_contact(id, &err);
    if (err.code != 0) {
        std::string msg(err.message ? err.message : "unknown error");
        int32_t code = err.code;
        weaveffi_error_clear(&err);
        throw WeaveFFIError(code, msg);
    }
    if (!result) return std::nullopt;
    return Contact(result);
}

inline std::vector<Contact> contacts_list_contacts() {
    size_t out_len = 0;
    weaveffi_error err{};
    auto result = weaveffi_contacts_list_contacts(&out_len, &err);
    if (err.code != 0) { /* ... throw ... */ }
    std::vector<Contact> ret;
    ret.reserve(out_len);
    for (size_t i = 0; i < out_len; ++i)
        ret.emplace_back(Contact(result[i]));
    return ret;
}

inline int32_t contacts_count_contacts() {
    weaveffi_error err{};
    auto result = weaveffi_contacts_count_contacts(&err);
    if (err.code != 0) { /* ... throw ... */ }
    return result;
}

} // namespace weaveffi
```

## Type mapping reference

| IDL type     | C++ type                             | Passed as parameter         |
|--------------|--------------------------------------|-----------------------------|
| `i32`        | `int32_t`                            | `int32_t`                   |
| `u32`        | `uint32_t`                           | `uint32_t`                  |
| `i64`        | `int64_t`                            | `int64_t`                   |
| `f64`        | `double`                             | `double`                    |
| `bool`       | `bool`                               | `bool`                      |
| `string`     | `std::string`                        | `const std::string&`        |
| `bytes`      | `std::vector<uint8_t>`               | `const std::vector<uint8_t>&` |
| `handle`     | `void*`                              | `void*`                     |
| `StructName` | `StructName`                         | `const StructName&`         |
| `EnumName`   | `EnumName` (`enum class`)            | `EnumName`                  |
| `T?`         | `std::optional<T>`                   | `const std::optional<T>&`   |
| `[T]`        | `std::vector<T>`                     | `const std::vector<T>&`     |
| `{K: V}`     | `std::unordered_map<K, V>`           | `const std::unordered_map<K, V>&` |

## Error handling via exceptions

Native errors are surfaced through a `WeaveFFIError` class that extends
`std::runtime_error`. Every generated function checks the C ABI error
struct after each call and throws on non-zero error codes:

```cpp
class WeaveFFIError : public std::runtime_error {
    int32_t code_;

public:
    WeaveFFIError(int32_t code, const std::string& msg)
        : std::runtime_error(msg), code_(code) {}
    int32_t code() const { return code_; }
};
```

When the IDL defines custom error codes, the generator also emits
specific exception subclasses (e.g. `NotFoundError`, `ValidationError`)
that inherit from `WeaveFFIError`, and the error-checking logic uses a
`switch` statement to throw the most specific type.

Catch errors in consumer code:

```cpp
try {
    auto contact = weaveffi::contacts_find_contact(42);
} catch (const weaveffi::WeaveFFIError& e) {
    std::cerr << "Error " << e.code() << ": " << e.what() << std::endl;
}
```

## Async support via `std::future`

Async IDL functions generate wrappers that return `std::future<T>`. Under
the hood, the wrapper creates a `std::promise`, passes a callback to the
C ABI `_async` function, and resolves (or rejects) the promise when the
callback fires:

```cpp
inline std::future<Contact> contacts_fetch_contact(int32_t id) {
    auto* promise_ptr = new std::promise<Contact>();
    auto future = promise_ptr->get_future();
    weaveffi_contacts_fetch_contact_async(id,
        [](void* context, weaveffi_error* err,
           weaveffi_contacts_Contact* result) {
            auto* p = static_cast<std::promise<Contact>*>(context);
            if (err && err->code != 0) {
                std::string msg(err->message ? err->message : "unknown error");
                p->set_exception(std::make_exception_ptr(
                    WeaveFFIError(err->code, msg)));
            } else {
                p->set_value(Contact(result));
            }
            delete p;
        }, static_cast<void*>(promise_ptr));
    return future;
}
```

Use it with `.get()` (blocking) or integrate with your application's
event loop:

```cpp
auto future = weaveffi::contacts_fetch_contact(42);
auto contact = future.get();
std::cout << contact.name() << std::endl;
```

Cancellable async functions accept an optional
`weaveffi_cancel_token*` parameter.

## CMake integration

The generated `CMakeLists.txt` defines an INTERFACE library target called
`weaveffi_cpp`:

```cmake
cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp)
add_library(weaveffi_cpp INTERFACE)
target_include_directories(weaveffi_cpp INTERFACE ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)
target_compile_features(weaveffi_cpp INTERFACE cxx_std_17)
```

To use in your project, add the generated directory as a subdirectory
and link your target:

```cmake
add_subdirectory(path/to/generated/cpp)
add_executable(myapp main.cpp)
target_link_libraries(myapp weaveffi_cpp)
```

This automatically adds the header include path, links the `weaveffi`
native library, and requires C++17. Then include the header:

```cpp
#include "weaveffi.hpp"
```

Make sure the Rust-built shared library (`libweaveffi.dylib`,
`libweaveffi.so`, or `weaveffi.dll`) is discoverable at link and
run time.
