# C++

The C++ target emits a header-only C++17 library over the C ABI. Records
become value structs, rich enums become `std::variant`-backed sum types,
interfaces become copyable RAII classes, callback interfaces become abstract
classes you subclass, error domains become exception hierarchies, async
functions return `std::future`, and `iter<T>` returns a lazy range.

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
named after the module path (`kvstore::kv::stats::get_stats`). Types sit at
the namespace root, since type names are unique across an API.

| `[cpp]` key | Default | Meaning |
|---|---|---|
| `namespace` | the C prefix | Namespace holding every declaration |
| `header_name` | `{library}.hpp` | Wrapper header file name |
| `standard` | `17` | C++ standard the CMake target requires |

## Build and link

Add the generated directory to a CMake build and link the INTERFACE target:

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

### The library check

`check_library()` verifies that the linked library implements C ABI revision
3 and the exact contract of every top-level module, and throws `LoadError`
naming the first mismatch:

```cpp
inline void check_library() {
    static const std::string failure = [] {
        uint32_t abi = kitchen_sink_abi_version();
        if (abi != KITCHEN_SINK_ABI_VERSION) {
            return "kitchen_sink: C ABI revision mismatch (header " + std::to_string(KITCHEN_SINK_ABI_VERSION) +
                   ", library " + std::to_string(abi) + ")";
        }
        if (kitchen_sink_shared_checksum() != 0x42c4ce2c8d0af052ull) {
            return std::string("kitchen_sink: module 'shared' does not match the linked library (contract checksum mismatch); regenerate the bindings for this build");
        }
        ...
```

A C++ binary links its library rather than loading it, and an exception
thrown during static initialization can't be caught, so the check runs on
first use instead: every free function, constructor, and static member calls
`check_library()` before its first native call. The result is cached, so the
cost after the first call is one guard-variable read. Call it at startup to
fail early.

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
| callback interface | abstract class | `std::shared_ptr<C>` |
| `T?`, `[T]`, `{K: V}` | `std::optional<T>`, `std::vector<T>`, `std::unordered_map<K, V>` | by const reference |
| `iter<T>` | lazy range class (returns only) | n/a |

Strings cross the ABI as a UTF-8 pointer and a length, so interior NULs
survive in both directions. A returned string is copied into a `std::string`
and the producer's allocation is released with `{prefix}_free_bytes`.
Records, rich enums, optionals, lists, and maps cross as one value buffer,
encoded and decoded by generated routines in `detail`.

## Objects and lifetime

An interface class holds one strong reference to a reference-counted
producer object:

```cpp
class Gadget {
    kitchen_sink_kitchen_Gadget* handle_;

public:
    /** The C type this class wraps. */
    using raw_type = kitchen_sink_kitchen_Gadget;

    /** Adopts one strong reference to a producer object: `Gadget(adopt, raw)`. */
    explicit Gadget(adopt_t, kitchen_sink_kitchen_Gadget* h) noexcept : handle_(h) {}
```

- The destructor releases the reference with `_destroy`; copying takes a new
  one with `_clone`, so copies share the object; moving transfers it.
- The constructor named `new` becomes an `explicit` C++ constructor; any
  other constructor becomes a static factory. Nothing converts implicitly
  into an object, and adopting a raw pointer requires the `adopt` tag.
- A parameter borrows the caller's reference for the call. A returned
  object, an async result, an iterator element, and an object argument
  handed to a callback are adopted.
- An object inside a record, list, map, or optional crosses as a token
  minted with `_clone`, so a record can be copied and destroyed freely.
- `handle()` borrows the C pointer; `clone_handle()` returns a new reference
  the caller owns.

The C++ object model makes lifetimes the caller's responsibility: a wrapper
must outlive the calls made through it, and destroying a wrapper on one
thread while another thread calls through it is a data race, as for any C++
object. A moved-from wrapper holds null.

## Errors

Every failure is an exception derived from `{namespace}::Error`, which
carries `code()`:

- Each module that declares an error domain gets a domain class
  (`KvError : Error`) and one subclass per code (`KeyNotFoundError`). A code
  with payload fields exposes them as members.
- A throwing function maps a positive code to its typed exception. Negative
  codes are runtime traps and always surface as the generic `Error`: -1
  generic, -2 producer panic, -3 marshalling failure, and -4 a callback
  implementation that threw.
- -5 surfaces as `Cancelled`.
- `LoadError` reports a library that doesn't match the headers.

Wrappers are never `noexcept`, since even a non-throwing function can
surface a trap.

## Async and cancellation

An async function returns `std::future<T>`, settled from a producer thread.
A `cancellable` function also takes a trailing `const CancelToken&`:

```cpp
inline std::future<std::string> do_cancellable(std::string_view input, const CancelToken& cancel_token = CancelToken::none()) {
```

`CancelToken` is a move-only RAII wrapper over the native token. Pass one
token to any number of calls and call `cancel()` from any thread; every call
still in flight completes with `Cancelled`. Each call takes its own native
reference, so the token can be destroyed at any time without affecting the
calls. Omitting the argument passes `CancelToken::none()`, which never
cancels.

```cpp
kvstore::CancelToken token;
std::future<int64_t> pending = store.compact(token);
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
class ReadyListener {
public:
    virtual ~ReadyListener() = default;

    /** Fires when an item is ready */
    virtual void on_ready(int32_t code, std::string_view msg) = 0;
```

Pass an implementation as `std::shared_ptr`. The wrapper boxes the pointer
as the vtable context, and the producer's last release deletes the box,
which runs your destructor if nothing else holds the implementation. The
producer may call methods from any thread, so implementations must be
thread-safe. String arguments are views valid for the call. An exception
thrown by a method is reported to the producer with code -4 and its
`what()` message; it never unwinds through the C frame. Whether the
producer method returns `T` or `Result<T, ForeignError>`, the caller sees
the generic `Error` with code -4.

## Iterators

An `iter<T>` function returns a move-only range class:

```cpp
for (const std::string& key : store.list_keys(std::nullopt)) {
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
- The C ABI's callback methods can't return strings or buffered values, so
  neither can the abstract classes.
