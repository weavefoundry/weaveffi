# Python

## Overview

The Python target produces pure-Python `ctypes` bindings, `.pyi` type
stubs, and packaging files over the C ABI (revision 2). Calls go
through Python's built-in `ctypes` module, so there's no compilation
step, no native extension, and no third-party runtime dependency. The
generated package works on any Python 3.7+ interpreter that can
`dlopen` the shared library.

Records and rich enums are dataclasses that cross the boundary as value
buffers; interfaces are reference-counted wrapper classes with `close()`
and a `__del__` backstop; callback interfaces are abstract base classes
the consumer subclasses; async functions surface as `async def` wrappers
integrated with asyncio; and `iter<T>` returns are lazy Python
iterators.

The trade-off is that `ctypes` calls are slower than compiled extensions
(`cffi`, `pybind11`, PyO3). For typical FFI workloads the overhead is
negligible compared to the work done inside the Rust library.

## What gets generated

| File | Purpose |
|------|---------|
| `python/<pkg>/__init__.py` | Re-exports the public API from `weaveffi.py` |
| `python/<pkg>/weaveffi.py` | ctypes bindings: library loader, ABI check, codecs, wrappers, classes |
| `python/<pkg>/weaveffi.pyi` | Type stub for IDE autocompletion and `mypy` |
| `python/pyproject.toml` | PEP 621 project metadata |
| `python/setup.py` | Fallback setuptools script |
| `python/README.md` | Basic usage instructions |

The package directory follows the IDL `package.name` (a package named
`kvstore` produces `python/kvstore/...`); `weaveffi` is the default and
`package_name` under `[generators.python]` overrides it.

The module verifies the producer's ABI revision at import time, so a
stale library fails fast instead of misreading the error struct or a
value buffer later:

```python
_ABI_VERSION = 2


def _check_abi_version(lib: ctypes.CDLL) -> None:
    try:
        fn = lib.weaveffi_abi_version
    except AttributeError:
        raise ImportError(
            "the loaded WeaveFFI library predates ABI versioning "
            f"(these bindings expect ABI revision {_ABI_VERSION})"
        ) from None
    fn.argtypes = []
    fn.restype = ctypes.c_uint32
    found = fn()
    if found != _ABI_VERSION:
        raise ImportError(
            f"WeaveFFI ABI mismatch: these bindings expect revision {_ABI_VERSION} "
            f"but the loaded library reports revision {found}"
        )
```

## Type mapping

| IDL type     | Python type hint     | ctypes type                        |
|--------------|----------------------|------------------------------------|
| `i8`, `i16`, `i32`, `i64` | `int`   | `c_int8`, `c_int16`, `c_int32`, `c_int64` |
| `u8`, `u16`, `u32`, `u64` | `int`   | `c_uint8`, `c_uint16`, `c_uint32`, `c_uint64` |
| `f32`, `f64` | `float`              | `c_float`, `c_double`              |
| `bool`       | `bool`               | `c_int32`                          |
| `string`     | `str`                | `c_char_p`                         |
| `bytes`      | `bytes`              | `POINTER(c_uint8)` + `c_size_t`    |
| `Struct`     | `"StructName"` (a `@dataclass`) | value buffer: `POINTER(c_uint8)` + `c_size_t` |
| `Enum` (plain) | `"EnumName"` (`IntEnum`) | `c_int32`                    |
| `Enum` (rich)  | `"EnumName"` (variant dataclasses) | value buffer, like `Struct` |
| `Interface`  | `"InterfaceName"` (wrapper class) | `c_void_p`            |
| `Interface?` | `Optional["InterfaceName"]` | `c_void_p` (NULL for `None`) |
| `CallbackInterface` | `"CallbackName"` (subclass of the generated ABC) | `c_void_p` ctx + `POINTER(<Name>Vtable)` |
| `T?`         | `Optional[T]`        | value buffer                       |
| `[T]`        | `List[T]`            | value buffer                       |
| `{K: V}`     | `Dict[K, V]`         | value buffer                       |
| `iter<T>`    | `Iterator[T]` (lazy) | opaque `c_void_p` iterator handle  |

Buffered types (structs, rich enums, optionals, lists, maps) cross the
boundary serialized in the
[value-buffer format](../reference/value-buffers.md); the generated
module ships a private `_BufferWriter`/`_BufferReader` pair plus one
pack and one unpack function per record and rich enum. Objects nested
inside a buffered value travel as object tokens (see
[Objects](#objects-interfaces)).

Booleans cross the boundary as `c_int32` (`0`/`1`) because C has no
standard fixed-width boolean type across ABIs.

### 64-bit integers and floats

Python's `int` is arbitrary precision, so `i64` and `u64` round-trip
exactly with no wrapper type: the `ctypes` slots are `c_int64` and
`c_uint64`, and the value-buffer codec packs them with `struct` as
`<q`/`<Q`. Out-of-range values raise from `ctypes`/`struct` before
anything crosses the boundary. `f32`/`f64` map to Python `float`; the
`codec` conformance consumer verifies that NaN, both infinities, and
`-0.0` survive a round trip bit-for-bit (an `f32` is still rounded to
single precision by the producer).

## Example IDL and generated code

```yaml
version: "0.9.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        doc: "Type of contact"
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        doc: "A contact record"
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
        return: Contact

      - name: count_contacts
        params: []
        return: i32
```

The generated module loads the platform-specific shared library:

```python
def _load_library() -> ctypes.CDLL:
    # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
    # specific build artifact regardless of its file name or location.
    override = os.environ.get("WEAVEFFI_LIBRARY")
    if override:
        return ctypes.CDLL(override)
    system = platform.system()
    if system == "Darwin":
        name = "libweaveffi.dylib"
    elif system == "Windows":
        name = "weaveffi.dll"
    else:
        name = "libweaveffi.so"
    return ctypes.CDLL(name)

_lib = _load_library()
```

Functions become snake_case Python functions with full type hints;
ctypes `argtypes`/`restype` are set up at the call site. A buffered
argument is packed with the generated codec; a buffered return is
decoded from the producer's buffer, which the wrapper then frees. From
the `kvstore` sample's `get_stats` (docstring trimmed):

```python
def get_stats(store: "Store") -> "Stats":
    _fn = _lib.weaveffi_kv_stats_get_stats
    _fn.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = ctypes.c_void_p
    _err = _WeaveFFIErrorStruct()
    _out_len = ctypes.c_size_t(0)
    _result = _fn(_borrow(store), ctypes.byref(_out_len), ctypes.byref(_err))
    _check_kv_error(_err)
    _data = _take_buffer(_result, _out_len.value)
    return _unpack_Stats(_data)
```

Enums become `IntEnum` subclasses:

```python
class ContactType(IntEnum):
    """Type of contact"""
    Personal = 0
    Work = 1
    Other = 2
```

Structs become plain `@dataclass` value classes. There are no C symbols
per struct: construction, equality, and repr come from the dataclass,
and instances cross the boundary serialized in value buffers by
generated private codec functions (`_pack_Contact` / `_unpack_Contact`):

```python
@dataclass
class Contact:
    """A contact record"""

    name: str
    email: Optional[str]
    age: int
```

The accompanying `.pyi` stub mirrors the public surface for IDE/mypy:

```python
class ContactType(IntEnum):
    Personal: int
    Work: int
    Other: int

class Contact:
    name: str
    email: Optional[str]
    age: int
    def __init__(self, name: str, email: Optional[str], age: int) -> None: ...

def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> "Contact": ...
```

Wrapper names drop the IDL module prefix by default and stay
snake_case, so `create_contact` in module `contacts` is exported as
plain `create_contact` (the C symbol keeps its full
`weaveffi_contacts_create_contact` name). Set
`strip_module_prefix: false` in the Python generator config (or under
`[global]`) to restore module-prefixed wrapper names like
`contacts_create_contact`.

## Typed errors

Every generated module defines `WeaveFFIError(Exception)` with `code`
and `message` attributes and the four runtime trap codes as class
constants:

```python
class WeaveFFIError(Exception):
    """An error reported through the C ABI.

    Positive codes are a module's declared error codes (raised as the
    module's typed subclasses); negative codes are runtime traps: a generic
    producer error (-1), a producer panic (-2), a marshalling failure (-3),
    or an exception raised by a callback-interface implementation (-4).
    """

    GENERIC_ERROR_CODE = -1
    PANIC_ERROR_CODE = -2
    MARSHAL_ERROR_CODE = -3
    FOREIGN_ERROR_CODE = -4

    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"({code}) {message}")
```

A module that declares an error domain also gets a domain base class
and one subclass per code, each pinning its stable `CODE`; from the
`kvstore` sample:

```python
class KvError(WeaveFFIError):
    """Base exception for the `kv` module's error domain."""


class KeyNotFound(KvError):
    """key not found"""

    CODE = 1

    def __init__(self, message: str = "key not found") -> None:
        super().__init__(1, message)
```

Only callables marked `throws: true` in the IDL raise these typed
errors: their wrappers check the error slot with `_check_kv_error`,
which maps the code through `_kv_error_from` and raises `KeyNotFound`,
`IoError`, or (for codes outside the domain, such as a producer panic)
a plain `WeaveFFIError`. Their docstrings carry a `Raises` section
naming the domain. A callable without `throws` uses the generic
`_check_error`, which raises `WeaveFFIError` only if the producer
misbehaves. The error slot's message and payload are copied and then
released with `weaveffi_error_clear` before the exception is raised.

```python
try:
    store.put("k", b"", EntryKind.Persistent, None)
except StoreFull:
    ...                      # specific code
except KvError as e:
    print(e.code, e.message) # any domain error
```

### Runtime error codes

| Code | Constant | Meaning | Where it surfaces |
|------|----------|---------|-------------------|
| `-1` | `GENERIC_ERROR_CODE` | The producer reported an error without a declared code, or the wrapper itself detected misuse (a `None` object pointer, a wrapper used after `close()`). | Raised as `WeaveFFIError` from the call. |
| `-2` | `PANIC_ERROR_CODE` | The Rust implementation panicked; the export macros and the async spawner catch the unwind. | Raised as `WeaveFFIError`, or set as an awaited future's exception. |
| `-3` | `MARSHAL_ERROR_CODE` | Malformed input at the boundary (invalid UTF-8, a truncated value buffer, a bad enum discriminant). | Raised as `WeaveFFIError`. |
| `-4` | `FOREIGN_ERROR_CODE` | A callback-interface method implemented in Python raised. | Raised as `WeaveFFIError` from the producer call that invoked the callback (see [Callback interfaces](#callback-interfaces)). |

Python has no non-raising call path: a non-throwing callable whose
error slot comes back non-zero still raises `WeaveFFIError`, so a
producer bug or a raising callback never goes unnoticed.

## Objects (interfaces)

An `interfaces:` entry becomes a Python class wrapping one strong
reference to a reference-counted producer object. Every interface
exports a `<Interface>_clone` and `<Interface>_destroy` pair on the C
ABI, bound once at module scope:

```python
_lib.weaveffi_kv_Store_clone.argtypes = [ctypes.c_void_p]
_lib.weaveffi_kv_Store_clone.restype = ctypes.c_void_p
_lib.weaveffi_kv_Store_destroy.argtypes = [ctypes.c_void_p]
_lib.weaveffi_kv_Store_destroy.restype = None
```

The wrapper class (from the `kvstore` sample, trimmed):

```python
class Store:
    @classmethod
    def _from_ptr(cls, ptr) -> "Store":
        """Adopt one strong reference the producer handed over."""
        _obj = cls.__new__(cls)
        _obj._ptr = ptr
        return _obj

    def __init__(self) -> None:
        raise TypeError("Store cannot be instantiated directly")

    def _clone_ptr(self):
        """A new strong reference to the same object (a raw pointer the
        receiver must eventually release), leaving this wrapper's own
        reference untouched."""
        return _lib.weaveffi_kv_Store_clone(_borrow(self))

    def close(self) -> None:
        """Release this wrapper's reference. The object itself is dropped
        when its last reference (here or in the producer) is released.
        Idempotent; the wrapper is unusable afterwards."""
        _p = getattr(self, "_ptr", None)
        self._ptr = None
        if _p is not None:
            _lib.weaveffi_kv_Store_destroy(_p)

    def __enter__(self) -> "Store":
        return self

    def __exit__(self, *exc) -> bool:
        self.close()
        return False

    def __del__(self) -> None:
        self.close()

    @classmethod
    def open(cls, path: str) -> "Store": ...
```

- **Construction.** A constructor named `new` renders as `__init__`
  (the `events` sample's `EventBus()`); any other constructor becomes a
  `@classmethod` factory (`Store.open("/tmp/cache.kv")`). An interface
  without a `new` constructor cannot be instantiated directly; its
  `__init__` raises `TypeError`. Methods are instance methods, statics
  are `@staticmethod`s, and deprecated members emit `DeprecationWarning`
  at call time.
- **Disposal.** `close()` releases the wrapper's reference through the
  `_destroy` symbol; it's idempotent, and the wrapper is a context
  manager (`with Store.open(path) as store:`). `__del__` is a backstop
  that closes an unclosed wrapper when the garbage collector reaches it.
  The producer object itself is dropped only when the last reference
  anywhere (any Python wrapper, the producer's own clones) is released.
- **Use after close.** `_borrow(obj)` is the only way a wrapper's
  pointer reaches the producer. On a closed wrapper it raises
  `WeaveFFIError(-1, "Store used after close()")`, whether the wrapper
  is the receiver, a parameter, or a field inside a record or list being
  packed:

  ```python
  def _borrow(obj):
      """The raw pointer of a live object wrapper, lent to the producer for the
      duration of one call. Raises if the wrapper was already closed."""
      _p = obj._ptr
      if _p is None:
          raise WeaveFFIError(-1, f"{type(obj).__name__} used after close()")
      return _p
  ```

- **Copies mint new references.** A method that returns the receiver or
  another existing object (`share()`, `fork()`) hands back a fresh
  strong reference, adopted by `_from_ptr` into a new wrapper; closing
  one wrapper never affects the other. The same holds for every object
  read out of a value buffer.

### Nullable objects, and objects inside values

An `Interface?` parameter is passed as NULL for `None` and an
`Interface?` return maps a NULL pointer to `None`:

```python
def larger(self, other: Optional["Store"]) -> Optional["Store"]:
    _fn = _lib.weaveffi_kv_Store_larger
    _fn.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = ctypes.c_void_p
    _err = _WeaveFFIErrorStruct()
    _result = _fn(_borrow(self), (_borrow(other) if other is not None else None), ctypes.byref(_err))
    _check_error(_err)
    if _result is None:
        return None
    return Store._from_ptr(_result)
```

Objects inside records, lists, maps, and optionals travel as 8-byte
object tokens in the value buffer. Writing a token mints a new strong
reference with `_clone_ptr()`, so the value can be dropped by the
producer without touching the wrapper's own reference; reading a token
adopts the reference into a fresh wrapper. From the `StoreInfo` record
(`store: Store`, `mirror: Store?`):

```python
def _write_StoreInfo(_w: _BufferWriter, value: "StoreInfo") -> None:
    _w.write_string(value.label)
    _w.write_object(value.store)
    if value.mirror is None:
        _w.write_option_flag(False)
    else:
        _w.write_option_flag(True)
        _w.write_object(value.mirror)
    _w.write_i64(value.count)


def _read_StoreInfo(_r: _BufferReader) -> "StoreInfo":
    return StoreInfo(
        label=_r.read_string(),
        store=Store._from_ptr(_r.read_object()),
        mirror=(Store._from_ptr(_r.read_object()) if _r.read_option_flag() else None),
        count=_r.read_i64(),
    )
```

`_BufferWriter.write_object` checks the wrapper is live immediately but
defers the actual clone to `finish()`, so an encoding that raises
part-way (a closed wrapper, a field of the wrong type) leaks no strong
references. `Store.open_many(paths) -> List[Store]` and
`Store.total_count(stores, extra)` in the `kvstore` sample show lists of
objects in both directions; each wrapper in a returned list owns its own
reference and must be closed (or left to `__del__`) individually.

Iterators over objects (`iter<Interface>`) adopt one reference per
`__next__`; async functions returning an object adopt the pointer inside
the completion callback and hand the wrapper to the awaiting coroutine.

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. Unlike a plain C-style `Enum`, which crosses the boundary as a
bare `ctypes.c_int32` discriminant, a rich enum crosses as a serialized
value buffer (`i32` tag, then the active variant's fields), and the
generator emits an idiomatic Python sum type with no FFI symbols
involved: a base class holding a nested `Tag` `IntEnum` and a `tag`
property, plus one `@dataclass` subclass per variant.

Given a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`:

```python
class Shape:
    """An algebraic shape (sum type with associated data)"""

    class Tag(IntEnum):
        Empty = 0
        Circle = 1
        Rectangle = 2
        Labeled = 3

    @property
    def tag(self) -> "Shape.Tag":
        """The discriminant of this value's active variant."""
        return type(self).TAG


@dataclass
class ShapeCircle(Shape):
    """A circle with a radius"""

    TAG = Shape.Tag.Circle

    radius: float
```

Each variant class is also aliased under the base class
(`Shape.Circle` is `ShapeCircle`), so consumers construct variants
directly and discriminate with `isinstance` or the `tag` property:

```python
from weaveffi import Shape, describe, scale

circle = Shape.Circle(2.0)
labeled = Shape.Labeled("unit", 3)

if isinstance(circle, Shape.Circle):
    print(circle.radius)             # 2.0
print(labeled.count)                 # 3

print(describe(circle))              # render via the C ABI
bigger = scale(circle, 3.0)          # returns a brand-new Shape value
```

Wrappers pack a `Shape` argument with the generated `_pack_Shape` codec
and unpack a returned buffer with `_unpack_Shape`, freeing the returned
buffer with `weaveffi_free_bytes`. Values are plain Python objects with
no native handle and no destructor. Variant fields of interface type
follow the object-token rules above. The `.pyi` stub mirrors the class
hierarchy (nested `Tag`, variant subclasses with typed fields) for IDE
and `mypy` support.

## Callback interfaces

A `callback_interfaces:` entry becomes an abstract base class. The
consumer subclasses it, implements every abstract method, and passes
an instance wherever the API takes that type. From the `kvstore`
sample:

```python
class EvictionListener(abc.ABC):
    """
    Consumer-implemented callback interface. Subclass it, implement every abstract method, and pass an instance wherever the API takes a `EvictionListener`; the producer may call the methods from any thread until it releases the instance. An exception raised by a method is reported to the producer as WeaveFFIError.FOREIGN_ERROR_CODE (-4) and aborts the call that was in progress.
    """

    @abc.abstractmethod
    def on_evict(self, entry: "Entry", reason: "EvictionReason") -> bool:
        """
        An entry left the store. Returns whether the listener wants to keep
        receiving notifications; `false` detaches it.
        """
```

```python
class Auditor(kvstore.EvictionListener):
    def on_evict(self, entry, reason):
        print(entry.key, reason)
        return True

store.set_eviction_listener(Auditor())
```

Under the hood there's exactly one process-wide C vtable per callback
interface, built from `ctypes.CFUNCTYPE` trampolines held at module
scope so their function pointers stay valid for the process lifetime.
Passing an implementation registers it in the module-level `_cb_impls`
dict and hands the producer the integer key as its `ctx`, so the
producer never holds a raw reference to a Python object:

```python
def _EvictionListener_on_evict_trampoline(ctx, entry_ptr, entry_len, reason, out_err):
    try:
        _impl = _cb_impls[ctx]
        _ret = _impl.on_evict(_unpack_Entry(ctypes.string_at(entry_ptr, entry_len) if entry_ptr else b""), EvictionReason(reason))
        return 1 if _ret else 0
    except Exception as _exc:
        # Never unwind through the C frame: report the failure and return a
        # default; the producer aborts its call with FOREIGN_ERROR_CODE.
        _lib.weaveffi_error_set(out_err, -4, str(_exc).encode("utf-8", "replace"))
        return 0


def _EvictionListener_vtable_free_trampoline(ctx):
    # The producer's last reference is gone; it never touches `ctx` again.
    _cb_impls.pop(ctx, None)
```

```python
def set_eviction_listener(self, listener: "EvictionListener") -> None:
    _fn = _lib.weaveffi_kv_Store_set_eviction_listener
    _fn.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(_EvictionListenerVtable), ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = None
    _listener_ctx = _cb_register(listener)
    _err = _WeaveFFIErrorStruct()
    _fn(_borrow(self), _listener_ctx, ctypes.byref(_EvictionListener_vtable), ctypes.byref(_err))
    _check_error(_err)
```

- **Lifetime.** The implementation stays registered (and therefore
  alive) exactly as long as the producer may call it: the entry is
  removed when the producer invokes the vtable's `free(ctx)`. A producer
  that retains the implementation (a store's eviction listener) keeps it
  alive across calls; one that doesn't (the `events` sample's
  `route_once`) frees it before the call returns. The same Python object
  may be passed more than once; each pass registers a new `ctx`.
- **Argument ownership.** Borrowed strings and buffers are copied before
  the method runs (`_unpack_Entry(ctypes.string_at(...))`), so the
  implementation may keep them. An object passed to a callback method is
  owned by the implementation: the trampoline adopts it with
  `_from_ptr`, so the method receives a full wrapper it may store, use
  after the callback returns, and must eventually `close()` (or leave to
  `__del__`). The `events` sample's `on_attached(bus)` demonstrates this;
  the received `EventBus` stays usable independently of the caller's
  wrapper.
- **Return values.** A method's return value is converted back to its C
  representation (`bool` to `0`/`1`, a plain enum to its discriminant, a
  record to a value buffer the producer frees).
- **Exceptions.** An exception escaping a method never unwinds through
  the C frame. The trampoline writes `FOREIGN_ERROR_CODE` (-4) with
  `str(exc)` as the message into the producer's error slot and returns a
  default value; the producer aborts the call that was in progress, and
  the original caller sees `WeaveFFIError` with `code == -4`. For a
  callable marked `throws`, the domain mapper passes -4 through
  unchanged (it's outside the domain), so `except KvError` doesn't catch
  it but `except WeaveFFIError` does. The interpreter is never taken
  down.
- **Threads.** The producer may call a method from any thread. `ctypes`
  acquires the GIL before entering a `CFUNCTYPE` trampoline, so a
  callback running on a producer thread is an ordinary Python call, but
  it runs on that thread: don't touch an asyncio loop or UI toolkit from
  it directly; use `loop.call_soon_threadsafe` or a queue. A blocking
  producer call that waits for a callback dispatched on another thread
  works only because the calling thread releases the GIL while inside
  `ctypes`; a callback that itself blocks on the calling thread will
  deadlock.

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate weaveffi.yaml -o generated --target python
   ```

2. Build the Rust shared library:

   ```bash
   cargo build --release -p your_library
   ```

3. Install the package (editable install for development):

   ```bash
   cd generated/python
   pip install -e .
   ```

4. Make the shared library findable at runtime. `WEAVEFFI_LIBRARY` may
   point at an exact file; otherwise the loader asks the dynamic linker
   for `libweaveffi.dylib` / `libweaveffi.so` / `weaveffi.dll`:

   - Any OS: `export WEAVEFFI_LIBRARY=$PWD/../../target/release/libweaveffi.dylib`
   - macOS: `export DYLD_LIBRARY_PATH=$PWD/../../target/release`
   - Linux: `export LD_LIBRARY_PATH=$PWD/../../target/release`
   - Windows: place `weaveffi.dll` next to your script or add its
     directory to `PATH`.

5. Use the bindings:

   ```python
   import asyncio
   from kvstore import EntryKind, Store

   with Store.open("/tmp/cache.kv") as store:
       store.put("alpha", b"\x01", EntryKind.Persistent, None)
       print(store.count(), Store.default_capacity())
       for key in store.list_keys(None):
           print(key)
       reclaimed = asyncio.run(store.compact())
   ```

## Packaging

`weaveffi package --target python` assembles one wheel source tree per
supplied desktop binary under `python/<platform-id>/`, each containing
the generated package, the native library copied next to `weaveffi.py`,
and a `setup.py` that ships it as package data and forces a
platform-tagged (non-pure) wheel. The packaged loader still honours
`WEAVEFFI_LIBRARY` first, then opens the bundled library if it exists,
then falls back to the system path. Only platforms with a wheel
platform tag are emitted:

| Platform | Wheel tag |
|----------|-----------|
| `macos-arm64` | `macosx_11_0_arm64` |
| `macos-x64` | `macosx_10_12_x86_64` |
| `linux-x64` | `manylinux2014_x86_64` |
| `linux-arm64` | `manylinux2014_aarch64` |
| `windows-x64` | `win_amd64` |

Android and `wasm32` binaries are skipped. Build each tree with
`python -m build --wheel` and tag the result with the platform tag
listed in its `README.md`. See [Packaging](../guides/packaging.md) for the
shared workflow.

## Memory and ownership

- **Strings in:** Python `str` is encoded to UTF-8 by `_string_to_bytes`
  before crossing the boundary. ctypes manages the lifetime of the
  temporary buffer.
- **Strings out:** owned `const char*` returns come back as raw
  addresses; `_take_string` copies the text and immediately calls
  `weaveffi_free_string` on the producer's buffer. `_bytes_to_string`
  handles borrowed strings, such as callback-method parameters, which
  the wrapper must not free.
- **Bytes:** copied in via a ctypes array, copied out via
  `ctypes.string_at`; the wrapper then releases the producer's buffer
  with `weaveffi_free_bytes`.
- **Buffered values out (structs, rich enums, optionals, lists, maps):**
  the wrapper decodes the returned value buffer into plain Python values
  (dataclasses, `dict`s, `list`s, `None`) with `_take_buffer`, which
  copies then releases the one buffer with `weaveffi_free_bytes`.
  Nothing is freed per element; object tokens are adopted into wrappers.
- **Buffered values in:** the wrapper packs the Python value with the
  generated codec into a `bytes` object it owns for the duration of the
  call; the producer never frees it. Object tokens inside it are fresh
  strong references the producer owns.
- **Interfaces:** one strong reference per wrapper, released by `close()`
  (or the `with` statement) with `__del__` as the backstop.
- **Callback implementations:** pinned in `_cb_impls` until the producer
  calls the vtable's `free`.

## Async support

Async IDL functions (`async: true`) are exposed as `async def` wrappers
that integrate directly with asyncio; no worker thread blocks waiting
for the result. The wrapper creates a future on the running loop, builds
a `ctypes.CFUNCTYPE` completion callback, calls the `_async`-suffixed C
launcher (which returns immediately), and awaits the future. From the
`kvstore` sample's `Store.compact`:

```python
async def compact(self) -> int:
    _fn = _lib.weaveffi_kv_Store_compact_async
    _loop = asyncio.get_running_loop()
    _fut = _loop.create_future()

    def _cb_impl(context, err, result):
        # Fires exactly once, on a producer thread: take ownership of
        # the result here, then hop back to the event loop.
        _state = {"err": None, "val": None}
        if err and err.contents.code != 0:
            _code = err.contents.code
            _msg = err.contents.message.decode("utf-8") if err.contents.message else ""
            _payload = ctypes.string_at(err.contents.payload_ptr, err.contents.payload_len) if err.contents.payload_ptr else b""
            _lib.weaveffi_error_free(err)
            _state["err"] = _kv_error_from(_code, _msg, _payload)
        else:
            try:
                _state["val"] = result
            except Exception as _exc:
                _state["err"] = _exc

        def _resolve():
            _async_pending.pop(_token, None)
            # A cancelled future must not be resolved.
            if _fut.cancelled():
                return
            if _state["err"] is not None:
                _fut.set_exception(_state["err"])
            else:
                _fut.set_result(_state["val"])

        _loop.call_soon_threadsafe(_resolve)

    _cb_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.POINTER(_WeaveFFIErrorStruct), ctypes.c_int64)
    _cb = _cb_type(_cb_impl)
    _token = _async_register(_cb)  # pinned until completion
    _fn.argtypes = [ctypes.c_void_p, ctypes.c_void_p, _cb_type, ctypes.c_void_p]
    _fn.restype = None
    _fn(self._ptr, None, _cb, None)
    return await _fut
```

The completion callback fires exactly once, on an arbitrary producer
thread. Result buffers passed to it (strings, bytes, and the serialized
value buffers of buffered results) are owned by the consumer, so the
wrapper copies or decodes them inside the callback and releases them
with `weaveffi_free_string` or `weaveffi_free_bytes`. A reported error
is heap-boxed and released with `weaveffi_error_free` after its fields
are copied. An object result is adopted into a wrapper with `_from_ptr`
inside the callback. Conversion happens on the producer thread; the
wrapper then hops back to the event loop with
`loop.call_soon_threadsafe` to resolve the future, since asyncio
futures must not be touched from foreign threads.

When the callable is marked `throws: true`, an error reported through
the callback is mapped through the domain mapper (here `_kv_error_from`)
and set as the future's exception, so `await` raises the typed error.
For a non-throwing callable a non-zero code can only be a producer bug
or a raising callback; the wrapper raises the generic `WeaveFFIError`
rather than swallowing it. A panic inside the spawned future surfaces
as `PANIC_ERROR_CODE` (-2).

Each callback trampoline is pinned in the module-level `_async_pending`
dict until completion, so the GC cannot collect an object the producer
still holds, even if the awaiting coroutine is cancelled. A cancelled
future is never resolved, but the native operation itself keeps
running.

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter; the Python wrapper always passes `None` (NULL)
for it, as in the `compact` example above. The token is not exposed, so
cancelling the awaiting asyncio task does not stop the native operation.
Cancellation tokens are currently surfaced only by the C and C++
targets.

## Iterators

Functions returning `iter<T>` receive an opaque iterator handle from the
C ABI and wrap it in a generated lazy iterator class. The wrapper
returns immediately; nothing is drained, and each consumer step issues
exactly one producer `next` call. The signature is annotated
`Iterator[T]`, so `for` loops, `list(...)`, and `next(...)` all work.
From the `kvstore` sample's `Store.list_keys`:

```python
class _StoreListKeysIterator:
    """Lazy iterator over a producer stream: each step pulls one element
    across the C boundary. The native handle is released exactly once, on
    exhaustion, on close(), or when the iterator is garbage collected."""

    def __iter__(self):
        return self

    def __next__(self):
        if self._done:
            raise StopIteration
        _next_fn = _lib.weaveffi_kv_Store_ListKeysIterator_next
        _next_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(_WeaveFFIErrorStruct)]
        _next_fn.restype = ctypes.c_int32
        _out_item = ctypes.c_void_p()
        _err = _WeaveFFIErrorStruct()
        _has = _next_fn(self._ptr, ctypes.byref(_out_item), ctypes.byref(_err))
        _check_kv_error(_err)
        if not _has:
            self._done = True
            self._destroy()
            raise StopIteration
        return _take_string(_out_item.value)

    def close(self):
        """Release the native iterator without draining it."""
        self._done = True
        self._destroy()

    def __del__(self):
        self._destroy()
```

The native handle is destroyed exactly once: eagerly on exhaustion, via
`close()` when iteration is abandoned early (a second `close()` is a
no-op), or from `__del__` as a garbage-collection backstop. `_destroy`
nulls the stored pointer, so a double destroy is impossible.

Each yielded element is owned by the consumer: strings are copied and
freed with `_take_string`, buffered elements are decoded and freed with
`_take_buffer`, and an object element is adopted into a wrapper with
`_from_ptr` (or `None` for a NULL `Interface?` element).

Errors from the launcher and from each `next` follow the function's
error strategy. A throwing iterator such as `list_keys` checks each step
with `_check_kv_error` and raises the typed domain error (`KeyNotFound`,
`IoError`, ...) from the step that failed; a non-throwing iterator
raises the generic `WeaveFFIError` only for producer bugs.

## Known limitations

- `ctypes` dispatch is slower than a compiled extension; hot loops
  crossing the boundary per element will feel it.
- Async cancellation doesn't propagate: cancelling the awaiting task
  leaves the native operation running, and `cancellable: true` tokens
  are not exposed.
- Callback methods run on whatever thread the producer uses, under the
  GIL; long-running callbacks stall the producer thread, and a callback
  that blocks waiting for the calling thread deadlocks.
- Interface wrappers without a `new` constructor can't be instantiated
  from Python; use the generated factories.
- The plain `generate` output relies on the dynamic linker (or
  `WEAVEFFI_LIBRARY`) to find the library; only `weaveffi package`
  bundles it.

## Troubleshooting

- **`OSError: cannot find ...`**: the loader could not locate the shared
  library. Set `WEAVEFFI_LIBRARY`, `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH`, or copy the library next to your script.
- **`ImportError: WeaveFFI ABI mismatch`**: the library was built by a
  different `weaveffi` release than the bindings. Regenerate the bindings
  and rebuild the library together.
- **`WeaveFFIError: (-1) Store used after close()`**: a closed wrapper
  was passed to the producer (as the receiver, a parameter, or a field
  of a record or list). Keep the wrapper open for as long as it's in
  use.
- **`WeaveFFIError: (-4) ...`**: a callback-interface method you
  implemented raised; the message is `str(exc)`. Catch the exception
  inside the method if the producer call should succeed anyway.
- **`WeaveFFIError: ...` with a positive code from a non-throwing
  call**: the Rust side returned a domain error the IDL didn't declare
  as `throws`; inspect `.code` / `.message`.
- **`TypeError: Store cannot be instantiated directly`**: the interface
  has no `new` constructor; call its `@classmethod` factory instead.
- **`RuntimeError: no running event loop`**: `async def` wrappers need a
  running loop; call them with `asyncio.run(...)` or from inside a
  coroutine.
