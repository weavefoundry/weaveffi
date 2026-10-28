# C++

The C++ target emits a header-only C++17 library over the
[C ABI](../reference/abi.md). Records become aggregates with memberwise
equality, rich enums become `std::variant`-backed sum types, interfaces
become copyable RAII classes, callback interfaces become abstract classes you
subclass, error domains become exception hierarchies rooted in one `Error`
class, async functions return `std::future`, and `iter<T>` returns a lazy
`Range<T>`.

C++ is a [Tier 1](../stability.md#target-tiers) target: it tracks every ABI
revision as it lands and runs the full conformance suite in CI. The header
compiles warning-free with `-Wall -Wextra -Werror` as C++17 (with
`-Wpedantic`) and as C++20.

## What gets generated

```text
cpp/
├── {library}.h      the C header (identical to the c target's)
├── {library}.hpp    the wrapper; includes {library}.h
├── CMakeLists.txt   INTERFACE target {library}::cpp
└── README.md
```

For the kvstore sample that's `kvstore.h`, `kvstore.hpp`, `CMakeLists.txt`,
and `README.md`. A library is one namespace, so everything in the wrapper
lives in one C++ namespace, named after the library (`kvstore`): free
functions (`kvstore::open_store`), types, and exceptions alike. Machinery
the wrapper uses internally lives in `{namespace}::detail`.

| `[generators.cpp]` key | Default | Meaning |
|---|---|---|
| `name` | the library name | Namespace holding every declaration (may be nested, as in `acme::kv`) |
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

`check_library()` verifies that the linked library implements the C ABI
revision the header was generated against and every declaration the header
was generated with, and throws `LoadError` naming the first mismatch. The
header embeds one contract row per declaration: the library's contract
function for it, the hash of its canonical signature, and its path, with
the signature as a comment:

```cpp
inline const ContractRow contract_rows[] = {
    {&kvstore_contract_dc5ad183936cb6fc, 0x1898683461a88009ull, "Store.get"}, // method get(string) -> Entry throws KvError
    ...
};
```

The check calls each contract function once and compares what it returns
with the row's hash. A declaration whose signature differs fails with
`kvstore: Store.get changed since these bindings were generated`. A C++
binary links its library rather than loading it, so a declaration the library
lacks fails to link instead. There's one row per function and member, type,
error domain, error code, callback interface, and callback method.
Declarations the library has and the header doesn't are fine, so a library
that adds functions, error codes, or callback methods keeps working with
older bindings.

An exception thrown during static initialization can't be caught, so the
check runs on first use instead: every free function, constructor, and
static member calls `check_library()` before its first native call. The
result is cached in a function-local static, so the cost after the first
call is one guard-variable read. Call it at startup to fail early;
`LoadError` derives from `Error` (with code 0), so one `catch` handles it
with everything else.

## Type mapping

| IDL type | C++ type | Parameter |
|---|---|---|
| `i8` ... `u64`, `f32`, `f64`, `bool` | `int8_t` ... `uint64_t`, `float`, `double`, `bool` | by value |
| `string` | `std::string` | `std::string_view` |
| `bytes` | `std::vector<uint8_t>` | `const std::vector<uint8_t>&` |
| C-style enum | `enum class E : int32_t` | by value |
| record | `struct R` (aggregate) | `const R&` |
| rich enum | `struct E` with `std::variant<...> value` | `const E&` |
| interface | RAII class | `const I&` (borrowed) |
| `I?` | `std::optional<I>` | `const std::optional<I>&` |
| callback interface, `Cb?` | abstract class | `std::shared_ptr<Cb>` (empty for none when optional) |
| `T?` of a scalar, `bool`, or C-style enum | `std::optional<T>` | by value |
| `[P]` of a numeric primitive | `std::vector<P>` | `const std::vector<P>&` |
| other `T?`, `[T]`, `{K: V}` | `std::optional<T>`, `std::vector<T>`, `std::unordered_map<K, V>` | by const reference |
| `iter<T>` | `Range<T>` (returns only) | n/a |

Every value crosses the ABI as one C value, in every position (a parameter,
a return, an async result, an iterator item, and a callback argument or
return):

- A string, bytes, a typed array (a list of a numeric primitive other than
  `u8`, such as `[i32]` or `[f64]`), and a value buffer cross as one
  `{prefix}_run`, a pointer and a byte length. An argument's run points at
  the argument's own storage (a string's UTF-8 without a terminator, so
  interior NULs survive, or a vector's elements), so nothing is copied. A
  returned run is copied into a `std::string` or `std::vector` and released
  with `{prefix}_run_free`.
- An optional scalar, `bool`, or C-style enum (`i64?`, `Mode?`) crosses as
  its `{prefix}_opt_*` struct of a presence flag and a value, never through
  a buffer.
- Records, rich enums, and every other optional, list, and map cross as one
  value buffer. The header has one `detail::write` and one `detail::read`
  overload per record and rich enum; optionals, vectors, and maps are encoded
  by generic templates in the runtime, so a new composite shape never adds
  code.
- An object crosses as a pointer, and a callback interface as its
  `{prefix}_{Cb}` struct of a context and a vtable (see
  [Callback interfaces](#callback-interfaces)).

Records are aggregates: build one with braces (`Entry{"key", value}`), and
every member not given takes its default member initializer (zero for
scalars and enums, empty otherwise), so a partial initializer compiles
without `-Wmissing-field-initializers` warnings. An interface wrapper has no
empty state, so a member holding one by value (or holding a record that
does) has no default, and a record with such a member has no default
constructor. Records and rich enums compare with `==` and `!=` memberwise:
floating-point members as IEEE values (a NaN member is unequal to itself),
and interface members by identity.

## Objects and lifetime

An interface class holds one strong reference to a reference-counted
producer object through a `detail::Handle`, so the class itself follows the
Rule of Zero:

```cpp
class Store {
public:
    using raw_type = kvstore_Store;
    explicit Store(adopt_t, raw_type* raw) noexcept : raw_(raw) {}
    ...
private:
    struct traits {
        using raw_type = kvstore_Store;
        static raw_type* clone(const raw_type* raw) noexcept { return kvstore_Store_clone(raw); }
        static void destroy(raw_type* raw) noexcept { kvstore_Store_destroy(raw); }
    };
    detail::Handle<traits> raw_;
};
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
- `==` and `!=` compare identity: two wrappers are equal when they share one
  producer object. `handle()` borrows the C pointer; `clone_handle()`
  returns a new reference the caller owns. An interface member named
  `handle`, `clone_handle`, `raw_type`, or `traits` gains a trailing `_`,
  and so does a free function or parameter named `adopt`, `adopt_t`,
  `check_library`, `detail`, or `std`
  ([reserved member names](../reference/naming.md#identifiers-in-generated-code)).

The C++ object model makes lifetimes the caller's responsibility: a wrapper
must outlive the calls made through it, and destroying a wrapper on one
thread while another thread calls through it is a data race, as for any C++
object. A moved-from wrapper holds null.

## Errors

Every exception the library throws derives from `{namespace}::Error`, which
derives from `std::runtime_error` and carries `code()`. Which class a failed
call throws depends on what it declares:

- `throws: SomeDomain`: each error domain gets a class (`KvError : Error`)
  and one subclass per code (`KeyNotFoundError : KvError`). The names come
  from the shared naming rule, which appends `Error` unless the IDL name
  already ends in it (the `KitchenErrors` domain is `KitchenError`). A code
  with payload fields exposes them as public members (`e.key`); a field
  named `code` or `what` gains a trailing `_` so it doesn't hide `code()` or
  `what()`. A declared code throws its class. Domains are open: a positive
  code these bindings don't know (a newer producer added it) throws the
  domain class itself, with the code and message intact. A library may
  declare several domains, and each call throws only its own, so two domains
  can reuse a code value.
- `throws: any`: a failure throws `Error` itself, with code -1 and the
  producer's message.
- Nothing declared: the call can still fail, but only through a producer
  bug, so it follows the
  [trap policy](../guides/errors-and-memory.md#the-trap-policy) and throws
  `InternalError` (an `Error`) with the runtime code and message.

On a call that declares errors, a runtime code throws the root `Error` with
that code (-2 producer panic, -3 marshalling failure, -4 a callback
implementation that failed); under every policy, -5 throws `Cancelled`. A
value buffer the wrapper can't decode is an `InternalError` with code -3, and
a required callback interface passed as an empty `std::shared_ptr` is an
`Error` with code -3, refused before the call. `LoadError` reports a library
that doesn't match the headers. Wrappers are never `noexcept`.

Error messages cross the ABI as a pointer and a length, so they keep any byte
the producer sent; an empty message is `""`. The wrapper releases a failed
call's message and payload once it has built the exception.

```cpp
try {
    store.get("missing");
} catch (const kvstore::KeyNotFoundError& e) {
    std::cerr << e.what() << " (" << e.key << ")\n";
} catch (const kvstore::KvError& e) {
    // Any other KvError code, known or not.
} catch (const kvstore::Error& e) {
    // A runtime failure (e.code() < 0).
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

An async result crosses like a return of its type: an optional scalar as its
`{prefix}_opt_*` struct, a typed array as a run. `std::future` has no
cancellation of its own, so destroying a future doesn't cancel the call;
cancel its token instead.

## Callback interfaces

A callback interface is an abstract class with one pure virtual method per
IDL method:

```cpp
class Policy {
public:
    virtual ~Policy() = default;

    virtual std::optional<int64_t> ttl_for(std::string_view key, std::optional<int64_t> requested) = 0;
    virtual Entry admit(const Entry& entry) = 0;
    virtual Store route(std::string_view key, Store home) = 0;
};
```

Pass an implementation as `std::shared_ptr`. The wrapper boxes the pointer
as the context of the `{prefix}_{Cb}` struct and pairs it with a static
vtable (from `detail::Callbacks<Policy>`) whose `size` is its own `sizeof`
and whose `free` deletes the box when the producer releases the
implementation, which runs your destructor if nothing else holds it. For an
optional callback parameter (`Loader?`), an empty `std::shared_ptr` (or
`nullptr`) passes a null vtable, which is none. The producer may call
methods, and release the implementation, from any thread, so
implementations must be thread-safe; the vtable never sets the thread-affine
flag. Vtables are open, so a library that appends a method keeps working
with an implementation generated before it: calling the method it lacks
fails in the producer with -4.

Arguments follow the parameter rules seen from your side: a string is a
`std::string_view` valid for the call, a typed array, bytes, or a buffered
value arrives by const reference to a copy, an optional scalar as
`std::optional<T>`, and an object by value as a wrapper you own.

A method can return any type. A scalar is returned as is and an optional
scalar as its `{prefix}_opt_*` struct; an object (`Store`,
`std::optional<Store>`) hands the producer a fresh reference; a string,
bytes, typed array, or buffered value is copied into a run allocated with
`{prefix}_alloc`, which the producer adopts. A required object return that's
null (a moved-from wrapper) and a value the producer can't decode are
refused by the producer with -3.

Each method reports an exception according to what it declares, and nothing
unwinds through the C frame:

- `throws: SomeDomain`: throw the domain's class or one of its codes'
  classes. The code, the message (`what()`), and the code's fields reach the
  producer, which decodes them into its own error type. Any other exception
  is a plain failure (code -1 with its `what()`).
- `throws: any`: any exception is a failure, code -1 with its `what()`.
- Nothing declared: any exception is a callback failure, -4.

```cpp
Entry admit(const Entry& entry) override {
    if (entry.key == "secret") throw kvstore::RejectedError("not stored", entry.key, "no secrets");
    return entry;
}
```

Whether the original call then fails is the producer's decision. A Rust
producer that propagates a typed failure with `?` makes the call throw the
same `RejectedError`, with the message the producer renders for it; the
kvstore sample turns any other callback failure into `CallbackFailedError`
carrying your exception's message.

## Iterators

An `iter<T>` function returns `Range<T>`, a lazy, single-pass, move-only
range:

```cpp
for (const std::string& key : store.keys(std::nullopt)) {
    // One producer call per step.
}
```

Each step pulls one element; an element crosses like a return of its type,
so `iter<i64?>` yields `std::optional<int64_t>` (an absent element isn't the
end) and `iter<[i32]>` yields `std::vector<int32_t>`. The range releases the
producer iterator exactly once: when it's exhausted, when a step throws, on
`close()`, or from the destructor, so abandoning a loop early leaks nothing.
`next()` returns `std::optional<T>` for manual iteration. The iterator type
models an input iterator with a sentinel end, so under C++20 a `Range<T>` is
a `std::ranges::input_range`.

## Known limitations

- Ranges are single-pass input ranges; copy them into a `std::vector` for
  random access.
- A typed array argument is a `const std::vector<T>&`; C++17 has no
  `std::span`, so other contiguous storage must be copied into a vector first.
- A callback interface must be a `std::shared_ptr` to a subclass; there's
  no lambda overload.
- C++ implementations don't declare their vtables thread-affine, so the
  producer may call any method from any thread.
- Exceptions not derived from `std::exception` are reported to the
  producer with a generic message.
- The library check runs on first use, not at load time (see above).
