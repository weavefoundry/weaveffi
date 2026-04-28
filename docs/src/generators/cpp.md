# C++

## Overview

The C++ target emits a header-only library `weaveffi.hpp` that wraps the
C ABI in idiomatic C++17. Structs become RAII classes with deleted copies
and movable handles, errors map to exceptions, and async functions return
`std::future`. A `CMakeLists.txt` is included so the generated directory
can be dropped into any CMake build.

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

## Example IDL → generated code

```yaml
version: "0.3.0"
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

Free functions live in the `weaveffi` namespace and throw on failure:

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
} // namespace weaveffi
```

`WeaveFFIError` extends `std::runtime_error`. When the IDL declares
custom error codes the generator also emits typed subclasses that the
exception dispatcher uses to throw the most specific exception:

```cpp
try {
    auto contact = weaveffi::contacts_find_contact(42);
} catch (const weaveffi::WeaveFFIError& e) {
    std::cerr << "Error " << e.code() << ": " << e.what() << '\n';
}
```

## Build instructions

The generated `CMakeLists.txt` defines an INTERFACE library:

```cmake
cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp)
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

- Struct wrappers own a single `void*` handle. The destructor calls the
  C `_destroy` function. Copies are deleted; moves transfer ownership
  by nulling the source handle.
- Strings returned from getters are copied into `std::string` and the
  raw pointer is freed via `weaveffi_free_string` before returning.
- Optional fields use `std::optional<T>`; a `nullptr` from the C layer
  becomes `std::nullopt`.
- `std::vector<T>` returns own their contents. List parameters borrow
  the underlying buffer for the duration of the call.

## Async support

Async IDL functions return `std::future<T>`. The wrapper allocates a
heap-owned `std::promise`, hands the C ABI a callback that resolves
(or rejects) the promise, and returns the corresponding future:

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

Use it with `.get()` (blocking) or compose with your event loop. When
the IDL marks the function `cancel: true`, the generated wrapper
forwards an additional `weaveffi_cancel_token*`.

## Troubleshooting

- **`undefined reference to weaveffi_*`** — link against the Rust
  cdylib. The header alone is not enough.
- **Double-free crashes** — RAII wrappers delete copy operators on
  purpose. If you see double-frees, somewhere you have a manual copy or
  a raw `void*` shared between wrappers.
- **Exceptions not caught across DLL boundaries on MSVC** — build the
  consumer and the dynamically loaded library with the same
  `_HAS_EXCEPTIONS` setting and CRT.
- **`std::optional` is missing** — the header requires C++17. Add
  `target_compile_features(... cxx_std_17)` to your CMake target.
