# C++

The C++ target emits a header-only C++17 library over the C ABI (revision
4). Records become value structs, rich enums become `std::variant`-backed
sum types, interfaces become copyable RAII classes, callback interfaces
become abstract classes you subclass, error domains become exception
hierarchies, async functions return `std::future`, and `iter<T>` returns a
lazy range.

## What gets generated

```text
cpp/
├── {library}.h      the C header (identical to the c target's)
├── {library}.hpp    the wrapper; includes {library}.h
├── CMakeLists.txt   INTERFACE target {library}::cpp
└── README.md
```

For the kvstore sample that's `kvstore.h`, `kvstore.hpp`, `CMakeLists.txt`,
and `README.md`. Everything in the wrapper lives in `namespace {prefix}`
(`kvstore`), and each IDL module's free functions live in a nested namespace
named after the module path (`kvstore::kv::stats::summarize`). Types sit at
the namespace root, since type names are unique across an API.

| `[generators.cpp]` key | Default | Meaning |
|---|---|---|
| `name` | the C prefix | Namespace holding every declaration |
| `header_name` | `{library}.hpp` | Wrapper header file name |
| `standard` | `17` | C++ standard the CMake target requires |

## Build and link

Add the generated directory to a CMake build (3.14 or later) and link the
INTERFACE target:

```cmake
add_subdirectory(path/to/cpp)
target_link_libraries(app PRIVATE kvstore::cpp)
```

The target adds the directory to the include path, requires C++17, and links
the producer library. Set the `KVSTORE_LIBRARY` CMake cache variable or
environment variable (in general `{PREFIX}_LIBRARY`) to a full path to link
that file; otherwise the target links `kvstore` by name, which resolves to a
CMake target of that name or `libkvstore` on the linker search path. Without
CMake, add `cpp/` to the include path and link `-lkvstore`.

Both headers can be included in one translation unit, in either order.

`weaveffi package` writes `kvstore-{version}-cpp.tar.gz`: both headers under
`include/`, each desktop platform's library under `lib/<platform>/`, a
README, and a `CMakeLists.txt` that links the host's library into
`kvstore::cpp`, so the packaged build ignores `KVSTORE_LIBRARY` (see
[Packaging](../guides/packaging.md)).

### The library check

`check_library()` verifies that the linked library implements C ABI revision
4 and every declaration the header was generated with, and throws
`LoadError` naming the first mismatch. The header embeds each top-level
module's contract table as `(id, hash, path)` entries and compares them with
the table the library returns from `{prefix}_{module}_contract`:

```cpp
inline void check_library() {
    static const std::string failure = []() -> std::string {
        uint32_t abi = kvstore_abi_version();
        if (abi != KVSTORE_ABI_VERSION) {
            return "kvstore: C ABI revision mismatch (header " + std::to_string(KVSTORE_ABI_VERSION) +
                   ", library " + std::to_string(abi) + ")";
        }
        if (std::string why = detail::contract_mismatch(kvstore_kv_contract, detail::kv_contract); !why.empty()) {
            return "kvstore: " + why;
        }
        ...
```

A declaration the library lacks fails with `kvstore: kv.Store.put is
missing from the library`, and one whose signature differs with
`kvstore: kv.Store.put changed since these bindings were generated`.
Declarations the library has and the header doesn't are fine, so a library
that adds functions keeps working with older bindings.

A C++ binary links its library rather than loading it, and an exception
thrown during static initialization can't be caught, so the check runs on
first use instead: every free function, constructor, and static member calls
`check_library()` before its first native call. The result is cached in a
function-local static, so the cost after the first call is one
guard-variable read. Call it at startup to fail early.

## Type mapping

| IDL type | C++ type | Parameter |
|---|---|---|
| `i8` ... `u64`, `f32`, `f64`, `bool` | `int8_t` ... `uint64_t`, `float`, `double`, `bool` | by value |
| `string` | `std::string` | `std::string_view` |
| `bytes` | `std::vector<uint8_t>` | `const std::vector<uint8_t>&` |
| C-style enum | `enum class E : int32_t` | by value |
| record | `struct R` | `const R&` |
| rich enum | `struct E` with `std::variant<...> value` | `const E&` |
| interface | RAII class | `const I&` (borrowed) |
| `I?` | `std::optional<I>` | `const std::optional<I>&` |
| callback interface, `Cb?` | abstract class | `std::shared_ptr<Cb>` (empty for none when optional) |
| `T?`, `[T]`, `{K: V}` | `std::optional<T>`, `std::vector<T>`, `std::unordered_map<K, V>` | by const reference |
| `iter<T>` | lazy range class (returns only) | n/a |

Strings cross the ABI as a UTF-8 pointer and a length, so interior NULs
survive in both directions. A returned string is copied into a `std::string`
and the producer's run is released with `{prefix}_free_bytes`. Records, rich
enums, optionals, lists, and maps cross as one value buffer. The header has
one writer and one reader per record, rich enum, and distinct composite type
(`detail::write_list_Entry`, `detail::read_map_string_Store`), and every call
site delegates to them.

## Objects and lifetime

An interface class holds one strong reference to a reference-counted
producer object:

```cpp
class Store {
    kvstore_kv_Store* raw_;

public:
    /** The C type this class wraps. */
    using raw_type = kvstore_kv_Store;

    /** Adopts one strong reference to a producer object: `Store(adopt, raw)`. */
    explicit Store(adopt_t, kvstore_kv_Store* h) noexcept : raw_(h) {}
```

- The destructor releases the reference with `_destroy`; copying takes a new
  one with `_clone`, so copies share the object; moving transfers it.
- The constructor named `new` becomes a C++ constructor (`explicit` when it
  takes arguments); any other constructor becomes a static factory
  (`Store::open(path)`). Nothing converts implicitly into an object, and
  adopting a raw pointer requires the `adopt` tag.
- A parameter borrows the caller's reference for the call. A returned
  object, an async result, an iterator element, and an object argument
  handed to a callback are adopted.
- An object inside a record, list, map, or optional crosses as a token
  minted with `_clone`, so a record can be copied and destroyed freely.
- `handle()` borrows the C pointer (compare two wrappers' handles to test
  identity); `clone_handle()` returns a new reference the caller owns. An
  interface member named `handle`, `clone_handle`, or `raw_type` gains a
  trailing `_` ([reserved member names](../reference/naming.md#identifiers-in-generated-code)).

The C++ object model makes lifetimes the caller's responsibility: a wrapper
must outlive the calls made through it, and destroying a wrapper on one
thread while another thread calls through it is a data race, as for any C++
object. A moved-from wrapper holds null.

## Errors

A call that declares errors (`throws`) throws an exception derived from
`{namespace}::Error`, which derives from `std::runtime_error` and carries
`code()`:

- Each module that declares an error domain gets a domain class
  (`KvError : Error`) and one subclass per code (`KeyNotFoundError`). Both
  names end in `Error`, which is appended when the IDL name lacks it (the
  `KitchenErrors` domain becomes `KitchenErrorsError`). A code with payload
  fields exposes them as public members (`e.key`).
- A positive code throws its typed subclass. A negative runtime code
  throws the root `Error` with that code: -1 generic, -2 producer panic, -3
  marshalling failure, and -4 a callback implementation that failed.
- -5 throws `Cancelled`, which also derives from `Error`.

A call that declares no errors can still fail, but only through a producer
bug, so it follows the [trap policy](../guides/errors-and-memory.md#the-trap-policy):
it throws `InternalError`, a `std::runtime_error` that isn't an `Error`,
whose `what()` names the runtime code and the producer's message and whose
`code()` and `message()` return them. A value buffer the wrapper can't
decode is also an `InternalError` with code -3.

`LoadError` (also a `std::runtime_error`) reports a library that doesn't
match the headers. Wrappers are never `noexcept`.

```cpp
try {
    store.get("missing");
} catch (const kvstore::KeyNotFoundError& e) {
    std::cerr << e.what() << " (" << e.key << ")\n";
} catch (const kvstore::KvError& e) {
    // Any other kv code.
}
```

## Async and cancellation

An async function returns `std::future<T>`, settled from a producer thread.
A `cancellable` function also takes a trailing `const CancelToken&`:

```cpp
std::future<uint32_t> compact(uint32_t pause_ms, const CancelToken& cancel_token = CancelToken::none()) const;
```

`CancelToken` is a move-only RAII wrapper over the native token. Pass one
token to any number of calls and call `cancel()` from any thread; every call
still in flight completes with `Cancelled`. Each call takes its own native
reference, so the token can be destroyed at any time without affecting the
calls. Omitting the argument passes `CancelToken::none()`, which never
cancels.

```cpp
kvstore::CancelToken token;
std::future<uint32_t> pending = store.compact(60000, token);
token.cancel();
try {
    pending.get();
} catch (const kvstore::Cancelled&) {
    // The runtime dropped the work.
}
```

`std::future` has no cancellation of its own, so destroying a future doesn't
cancel the call; cancel its token instead.

## Callback interfaces

A callback interface is an abstract class with one pure virtual method per
IDL method:

```cpp
class Policy {
public:
    virtual ~Policy() = default;

    virtual Entry admit(const Entry& entry) = 0;
    virtual Store route(std::string_view key, Store home) = 0;
};
```

Pass an implementation as `std::shared_ptr`. The wrapper boxes the pointer
as the vtable context and hands the producer a static vtable whose header
records its `size` and whose `free` deletes the box when the producer
releases the implementation, which runs your destructor if nothing else
holds it. For an optional callback parameter (`Loader?`), an empty
`std::shared_ptr` (or `nullptr`) passes none; a required one throws
`std::invalid_argument` when empty. The producer may call methods, and
release the implementation, from any thread, so implementations must be
thread-safe.

Arguments follow the parameter rules seen from your side: a string is a
`std::string_view` valid for the call, bytes and buffered values arrive by
const reference to a decoded copy, and an object arrives by value as a
wrapper you own.

A method can return any family. A direct value is returned as is; an object
(`Store`, `std::optional<Store>`) hands the producer a fresh reference; a
string, bytes, or buffered value is copied into a run allocated with
`{prefix}_alloc`, which the producer adopts. A required object return that's
null (a moved-from wrapper) is refused by the producer with -3, as is a
record the producer can't decode.

A method declared `throws` may throw its module's domain exception: the
code, the message (`what()`), and the code's fields reach the producer,
which decodes them into its own error type.

```cpp
Entry admit(const Entry& entry) override {
    if (entry.key == "secret") throw kvstore::RejectedError("secrets are not stored", entry.key, "no secrets");
    return entry;
}
```

Any other exception, or a domain exception from a method that doesn't
declare errors, reaches the producer as a callback failure (-4) with the
exception's `what()`; nothing unwinds through the C frame. Whether the
original call then fails is the producer's decision: a Rust producer that
propagates the failure with `?` makes a throwing call throw the same
`RejectedError` (or the root `Error` with code -4).

## Iterators

An `iter<T>` function returns a move-only range class:

```cpp
for (const std::string& key : store.keys(std::nullopt)) {
    // One producer call per step.
}
```

Each step pulls one element. The range releases the producer iterator when
it's exhausted or destroyed, so abandoning a loop early leaks nothing.
`next()` returns `std::optional<T>` for manual iteration.

## Known limitations

- The ranges are single-pass input ranges; copy them into a `std::vector`
  for random access.
- A callback interface must be a `std::shared_ptr` to a subclass; there's
  no lambda overload.
- Exceptions not derived from `std::exception` are reported to the
  producer with a generic message.
- The library check runs on first use, not at load time (see above).
