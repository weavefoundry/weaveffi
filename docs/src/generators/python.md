# Python

## Overview

The Python target produces pure-Python ctypes bindings, type stubs, and
packaging files. Calls go through Python's built-in `ctypes` module so
there is no compilation step, no native extension, and no third-party
runtime dependency. The generated package works on any Python 3.7+
interpreter that can `dlopen` the shared library.

The trade-off is that ctypes calls are slower than compiled extensions
(`cffi`, `pybind11`, PyO3). For typical FFI workloads the overhead is
negligible compared to the work done inside the Rust library.

## What gets generated

| File | Purpose |
|------|---------|
| `python/weaveffi/__init__.py` | Re-exports the public API from `weaveffi.py` |
| `python/weaveffi/weaveffi.py` | ctypes bindings: library loader, wrappers, classes |
| `python/weaveffi/weaveffi.pyi` | Type stub for IDE autocompletion and `mypy` |
| `python/pyproject.toml` | PEP 621 project metadata |
| `python/setup.py` | Fallback setuptools script |
| `python/README.md` | Basic usage instructions |

The package directory follows the IDL `package.name` (a package named
`events` produces `python/events/...`); `weaveffi` is the default.

## Type mapping

| IDL type     | Python type hint     | ctypes type                        |
|--------------|----------------------|------------------------------------|
| `i32`        | `int`                | `ctypes.c_int32`                   |
| `u32`        | `int`                | `ctypes.c_uint32`                  |
| `i64`        | `int`                | `ctypes.c_int64`                   |
| `f64`        | `float`              | `ctypes.c_double`                  |
| `i8`         | `int`                | `ctypes.c_int8`                    |
| `i16`        | `int`                | `ctypes.c_int16`                   |
| `u8`         | `int`                | `ctypes.c_uint8`                   |
| `u16`        | `int`                | `ctypes.c_uint16`                  |
| `u64`        | `int`                | `ctypes.c_uint64`                  |
| `f32`        | `float`              | `ctypes.c_float`                   |
| `bool`       | `bool`               | `ctypes.c_int32`                   |
| `string`     | `str`                | `ctypes.c_char_p`                  |
| `bytes`      | `bytes`              | `ctypes.POINTER(ctypes.c_uint8)` + `ctypes.c_size_t` |
| `handle`     | `int`                | `ctypes.c_uint64`                  |
| `Struct`     | `"StructName"`       | `ctypes.c_void_p`                  |
| `Interface`  | `"InterfaceName"`    | `ctypes.c_void_p`                  |
| `Enum` (plain) | `"EnumName"`       | `ctypes.c_int32`                   |
| `Enum` (rich)  | `"EnumName"`       | `ctypes.c_void_p`                  |
| `T?`         | `Optional[T]`        | `ctypes.POINTER(scalar)` for values; same pointer for strings/structs |
| `[T]`        | `List[T]`            | `ctypes.POINTER(scalar)` + `ctypes.c_size_t` |
| `{K: V}`     | `Dict[K, V]`         | key/value pointer arrays + `ctypes.c_size_t` |
| `iter<T>`    | `Iterator[T]` (lazy) | opaque `ctypes.c_void_p` iterator handle |

Booleans cross the boundary as `c_int32` (`0`/`1`) because C has no
standard fixed-width boolean type across ABIs.

## Example IDL → generated code

```yaml
version: "0.5.0"
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
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
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
ctypes `argtypes`/`restype` are set up at the call site:

```python
def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int:
    _fn = _lib.weaveffi_contacts_create_contact
    _fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int32,
                    ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = ctypes.c_uint64
    _err = _WeaveFFIErrorStruct()
    _result = _fn(_string_to_bytes(name), _string_to_bytes(email),
                  contact_type.value, ctypes.byref(_err))
    _check_error(_err)
    return _result
```

Enums become `IntEnum` subclasses:

```python
class ContactType(IntEnum):
    """Type of contact"""
    Personal = 0
    Work = 1
    Other = 2
```

Structs become Python classes that wrap a void pointer and expose
`@property` getters; `__del__` calls the C destructor:

```python
class Contact:
    """A contact record"""

    def __init__(self, _ptr: int) -> None:
        self._ptr = _ptr

    def __del__(self) -> None:
        if self._ptr is not None:
            _lib.weaveffi_contacts_Contact_destroy.argtypes = [ctypes.c_void_p]
            _lib.weaveffi_contacts_Contact_destroy.restype = None
            _lib.weaveffi_contacts_Contact_destroy(self._ptr)
            self._ptr = None

    @property
    def name(self) -> str:
        _fn = _lib.weaveffi_contacts_Contact_get_name
        _fn.argtypes = [ctypes.c_void_p]
        _fn.restype = ctypes.c_void_p
        return _take_string(_fn(self._ptr)) or ""
```

String getters return the C string as a raw address; `_take_string`
copies it into a Python `str` and frees the producer's buffer with
`weaveffi_free_string`, so the getter doesn't leak.

The accompanying `.pyi` stub mirrors the public surface for IDE/mypy:

```python
class ContactType(IntEnum):
    Personal: int
    Work: int
    Other: int

class Contact:
    @property
    def name(self) -> str: ...
    @property
    def email(self) -> Optional[str]: ...
    @property
    def age(self) -> int: ...

def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int: ...
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
and `message` attributes. A module that declares an error domain also
gets a domain base class and one subclass per code, each pinning its
stable `CODE`; from the `contacts` sample:

```python
class ContactsError(WeaveFFIError):
    """Base exception for the `contacts` module's error domain."""


class InvalidName(ContactsError):
    """name must not be empty"""

    CODE = 1

    def __init__(self, message: str = "name must not be empty") -> None:
        super().__init__(1, message)


class NotFound(ContactsError):
    """contact not found"""

    CODE = 2

    def __init__(self, message: str = "contact not found") -> None:
        super().__init__(2, message)


ContactsError.InvalidName = InvalidName
ContactsError.NotFound = NotFound
```

Only callables marked `throws: true` in the IDL raise these typed
errors: their wrappers check the error slot with
`_check_contacts_error`, which maps the code through
`_contacts_error_from` and raises `NotFound`, `InvalidName`, or (for
codes outside the domain, such as producer panics) a plain
`WeaveFFIError`. Their docstrings carry a `Raises` section naming the
domain. A callable without `throws` uses the generic `_check_error`,
which raises `WeaveFFIError` only if the producer misbehaves:

```python
try:
    contact = book.get(999)
except NotFound:
    ...                      # specific code
except ContactsError as e:
    print(e.code, e.message) # any domain error
```

## Interfaces

An `interfaces:` entry becomes a Python class wrapping the opaque
pointer. A constructor named `new` renders as `__init__`; any other
constructor becomes a `@classmethod` factory. Methods are instance
methods, statics are `@staticmethod`s, and `__del__` calls the C
destructor; `_from_ptr` builds an instance around a pointer the C side
already owns. From the `kvstore` sample (trimmed):

```python
class Store:
    """An embedded key-value store owning its entries"""

    @classmethod
    def _from_ptr(cls, ptr) -> "Store":
        _obj = cls.__new__(cls)
        _obj._ptr = ptr
        return _obj

    def __del__(self) -> None:
        if self._ptr is not None:
            _lib.weaveffi_kv_Store_destroy.argtypes = [ctypes.c_void_p]
            _lib.weaveffi_kv_Store_destroy.restype = None
            _lib.weaveffi_kv_Store_destroy(self._ptr)
            self._ptr = None

    @classmethod
    def open(cls, path: str) -> "Store":
        """Open (or create) a store backed by the given filesystem path

        Raises
        ------
        KvError
            If the call reports one of the domain's error codes.
        """
        _fn = _lib.weaveffi_kv_Store_open
        _fn.argtypes = [ctypes.c_char_p, ctypes.POINTER(_WeaveFFIErrorStruct)]
        _fn.restype = ctypes.c_void_p
        _err = _WeaveFFIErrorStruct()
        _result = _fn(_string_to_bytes(path), ctypes.byref(_err))
        _check_kv_error(_err)
        if _result is None:
            raise WeaveFFIError(-1, "null pointer")
        return cls._from_ptr(_result)

    def get(self, key: str) -> Optional["Entry"]: ...
    def delete(self, key: str) -> bool: ...

    async def compact(self) -> int:
        _fn = _lib.weaveffi_kv_Store_compact_async
        _loop = asyncio.get_running_loop()
        _fut = _loop.create_future()
        # ... completion callback resolves _fut via call_soon_threadsafe ...
        return await _fut

    def legacy_put(self, key: str, value: bytes) -> bool:
        import warnings
        warnings.warn("use put() with explicit kind", DeprecationWarning, stacklevel=2)
        ...

    @staticmethod
    def default_capacity() -> int: ...
```

A constructor named `new` (as on the `contacts` sample's `ContactBook`)
lets you write `book = ContactBook()`; named constructors read as
`store = Store.open("/tmp/cache.kv")`. Methods on the C ABI take the
receiver as the leading argument (`weaveffi_kv_Store_put(self._ptr,
...)`), and functions elsewhere in the IDL accept or return the wrapper
directly (`get_stats(store)` passes `store._ptr`). Deprecated members
emit `DeprecationWarning` at call time.

```python
import asyncio
from kvstore import EntryKind, Store

store = Store.open("/tmp/cache.kv")
store.put("alpha", b"\x01", EntryKind.Persistent, None)
print(store.count(), Store.default_capacity())
reclaimed = asyncio.run(store.compact())
```

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. Unlike a plain C-style `Enum`, which crosses the boundary as a
bare `ctypes.c_int32` discriminant, a rich enum lowers to an **opaque
object handle**, so the generator emits a wrapper class with exactly the
same ownership model as a struct wrapper: a `ctypes.c_void_p` held
behind `@property` accessors and freed by `__del__`.

Given a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`, the generated class exposes a nested `Tag` `IntEnum`, one
`@classmethod` constructor per variant, a `tag` property, and a
per-variant field getter for each payload:

```python
class Shape:
    """An algebraic shape (sum type with associated data)"""

    class Tag(IntEnum):
        Empty = 0
        Circle = 1
        Rectangle = 2
        Labeled = 3

    def __del__(self) -> None:
        if self._ptr is not None:
            _lib.weaveffi_shapes_Shape_destroy.argtypes = [ctypes.c_void_p]
            _lib.weaveffi_shapes_Shape_destroy.restype = None
            _lib.weaveffi_shapes_Shape_destroy(self._ptr)
            self._ptr = None

    @property
    def tag(self) -> int:
        _fn = _lib.weaveffi_shapes_Shape_tag
        _fn.argtypes = [ctypes.c_void_p]
        _fn.restype = ctypes.c_int32
        return _fn(self._ptr)

    @classmethod
    def circle(cls, radius: float) -> "Shape":
        """A circle with a radius"""
        _fn = _lib.weaveffi_shapes_Shape_Circle_new
        _fn.argtypes = [ctypes.c_double, ctypes.POINTER(_WeaveFFIErrorStruct)]
        _fn.restype = ctypes.c_void_p
        _err = _WeaveFFIErrorStruct()
        _result = _fn(radius, ctypes.byref(_err))
        _check_error(_err)
        if _result is None:
            raise WeaveFFIError(-1, "null pointer")
        return cls(_result)

    @property
    def circle_radius(self) -> float:
        """Radius in points"""
        _fn = _lib.weaveffi_shapes_Shape_Circle_get_radius
        _fn.argtypes = [ctypes.c_void_p]
        _fn.restype = ctypes.c_double
        return _fn(self._ptr)
```

The full surface mirrors the variants: constructors `Shape.empty()`,
`Shape.circle(radius)`, `Shape.rectangle(width, height)`, and
`Shape.labeled(label, count)` (the last takes `ctypes.c_char_p` +
`ctypes.c_uint8`); field getters `circle_radius`, `rectangle_width`,
`rectangle_height`, `labeled_label`, and `labeled_count`. Each C symbol
follows the `weaveffi_shapes_Shape_<Variant>_new` /
`weaveffi_shapes_Shape_<Variant>_get_<field>` pattern, with
`weaveffi_shapes_Shape_tag` reading the discriminant.

Construct a couple of variants, read the tag and a field, then hand the
wrapper to a free function:

```python
from weaveffi import Shape, describe, scale

circle = Shape.circle(2.0)
labeled = Shape.labeled("unit", 3)

if circle.tag == Shape.Tag.Circle:
    print(circle.circle_radius)      # 2.0
print(labeled.labeled_count)         # 3

print(describe(circle))              # render via the C ABI
bigger = scale(circle, 3.0)          # returns a brand-new Shape
```

**Ownership:** each `Shape` owns its `ctypes.c_void_p`; `__del__` calls
`weaveffi_shapes_Shape_destroy` once the last Python reference is
dropped, and the `Shape` returned by `scale` is owned the same
way. The `.pyi` stub mirrors the class (nested `Tag`, `@classmethod`
constructors, and `@property` getters) for IDE and `mypy` support.

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

4. Make the shared library findable at runtime:

   - macOS: `export DYLD_LIBRARY_PATH=$PWD/../../target/release`
   - Linux: `export LD_LIBRARY_PATH=$PWD/../../target/release`
   - Windows: place `weaveffi.dll` next to your script or add its
     directory to `PATH`.

5. Use the bindings:

   ```python
   from weaveffi import (
       ContactType,
       count_contacts,
       create_contact,
       get_contact,
   )

   handle = create_contact("Alice", "alice@example.com", ContactType.Work)
   contact = get_contact(handle)
   print(f"{contact.name} ({contact.email})")
   print(f"Total: {count_contacts()}")
   ```

## Memory and ownership

- **Strings in:** Python `str` is encoded to UTF-8 by `_string_to_bytes`
  before crossing the boundary. ctypes manages the lifetime of the
  temporary buffer.
- **Strings out:** owned `const char*` returns come back as raw
  addresses; `_take_string` copies the text and immediately calls
  `weaveffi_free_string` on the producer's buffer. (`_bytes_to_string`
  is reserved for borrowed strings, such as listener callback
  parameters, which the wrapper must not free.)
- **Bytes:** copied in via a ctypes array, copied out via slicing
  (`_result[:_out_len.value]`); the wrapper then releases the
  producer's buffer with `weaveffi_free_bytes`.
- **Optional scalars out:** the producer boxes the value behind a
  pointer (null means `None`); the wrapper dereferences it and frees
  the box with `weaveffi_free_bytes`.
- **Lists and maps out:** each element is copied (string elements
  through `_take_string`, which frees them individually), then the
  array buffer itself, or both parallel key/value buffers for a map,
  is released with `weaveffi_free_bytes`.
- **Structs:** wrappers hold an opaque `c_void_p`. `__del__` calls the
  matching `_destroy` C function. For deterministic cleanup, use the
  `_PointerGuard` context manager:

  ```python
  with _PointerGuard(handle, _lib.weaveffi_contacts_Contact_destroy):
      ...
  ```

## Async support

Async IDL functions (`async: true`) are exposed as `async def`
wrappers that integrate directly with asyncio; no worker thread blocks
waiting for the result. The wrapper creates a future on the running
loop, builds a `ctypes.CFUNCTYPE` completion callback, calls the
`_async`-suffixed C launcher (which returns immediately), and awaits
the future. From the `kvstore` sample's `Store.compact`:

```python
async def compact(self) -> int:
    """Reclaim space asynchronously; returns the number of bytes reclaimed

    Raises
    ------
    KvError
        If the call reports one of the domain's error codes.
    """
    _fn = _lib.weaveffi_kv_Store_compact_async
    _loop = asyncio.get_running_loop()
    _fut = _loop.create_future()

    def _cb_impl(context, err, result):
        # Fires exactly once, on a producer thread: convert (copying
        # borrowed buffers) here, then hop back to the event loop.
        _state = {"err": None, "val": None}
        if err and err.contents.code != 0:
            _code = err.contents.code
            _msg = err.contents.message.decode("utf-8") if err.contents.message else ""
            _lib.weaveffi_error_clear(ctypes.byref(err.contents))
            _state["err"] = _kv_error_from(_code, _msg)
        else:
            _state["val"] = result

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
thread. Result buffers passed to it (strings, bytes, arrays) are owned
by the producer and valid only for the callback's duration, so the
wrapper deep-copies them inside the callback and never frees them.
Owned-object results (structs, rich enums, interfaces, including
optional ones) are the exception: the callback receives ownership and
adopts the pointer into a wrapper class. Conversion happens on the
producer thread; the wrapper then hops back to the event loop with
`loop.call_soon_threadsafe` to resolve the future, since asyncio
futures must not be touched from foreign threads.

When the callable is marked `throws: true`, an error reported through
the callback is mapped through the domain mapper (here
`_kv_error_from`) and set as the future's exception, so `await` raises
the typed error. For a non-throwing callable a non-zero code can only
be a producer bug; the wrapper raises the generic `WeaveFFIError`
rather than swallowing it.

Each callback trampoline is pinned in the module-level `_async_pending`
dict until completion, so the GC cannot collect an object the producer
still holds, even if the awaiting coroutine is cancelled. A cancelled
future is never resolved, but the native operation itself keeps
running.

Async interface methods work the same way as bound methods: the
receiver pointer is passed as the launcher's leading argument.

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter; the Python wrapper always passes `None` (NULL)
for it, as in the `compact` example above. The token is not exposed,
so cancelling the awaiting asyncio task does not stop the native
operation. Cancellation tokens are currently surfaced only by the C
and C++ targets.

## Callbacks and listeners

IDL `callbacks` declare a C function-pointer type; a `listener` pairs
one with register/unregister entry points:

```yaml
callbacks:
  - name: OnMessage
    params:
      - { name: message, type: string }
listeners:
  - name: message_listener
    event_callback: OnMessage
```

Each listener becomes a register/unregister pair of module functions.
Registering wraps the Python callable in a `ctypes.CFUNCTYPE`
trampoline that decodes each C slot, and returns a `uint64`
subscription id:

```python
_CFUNC_weaveffi_events_OnMessage_fn = ctypes.CFUNCTYPE(
    None, ctypes.c_char_p, ctypes.c_void_p)


def register_message_listener(callback: Callable[[str], None]) -> int:
    def _trampoline(message, _context):
        callback(_bytes_to_string(message))
    _cfunc = _CFUNC_weaveffi_events_OnMessage_fn(_trampoline)
    _fn = _lib.weaveffi_events_register_message_listener
    _fn.argtypes = [_CFUNC_weaveffi_events_OnMessage_fn, ctypes.c_void_p]
    _fn.restype = ctypes.c_uint64
    _listener_id = int(_fn(_cfunc, None))
    _listener_refs[_listener_id] = _cfunc
    return _listener_id


def unregister_message_listener(listener_id: int) -> None:
    _fn = _lib.weaveffi_events_unregister_message_listener
    # ...
    _fn(ctypes.c_uint64(listener_id))
    _listener_refs.pop(listener_id, None)
```

- **GC safety**: the ctypes function object is pinned in the
  module-level `_listener_refs` dict, keyed by subscription id, so the
  garbage collector cannot reclaim a trampoline the producer may still
  call. Unregistering drops the reference.
- **Subscription ids**: registration returns the `uint64` id produced
  by `weaveffi_events_register_message_listener(fn, context)`; pass it
  to `unregister_message_listener` to stop delivery and release
  the trampoline.
- **Threading**: the callback fires on the producer's thread, not the
  thread that registered it. Do not block inside it; if results must
  reach an asyncio loop or UI thread, marshal them yourself (e.g. with
  `loop.call_soon_threadsafe`).

Typical round trip:

```python
listener_id = register_message_listener(lambda m: print(m))
send_message("hello")
unregister_message_listener(listener_id)
```

## Iterators

Functions returning `iter<T>` receive an opaque iterator handle from
the C ABI (`weaveffi_events_get_messages`) and wrap it in a generated
lazy iterator class. The wrapper returns immediately; nothing is
drained, and each consumer step issues exactly one producer `next`
call (`weaveffi_events_GetMessagesIterator_next`). The signature is
annotated `Iterator[str]`:

```python
def get_messages() -> Iterator[str]:
    """
    Return an iterator over all sent messages

    Returns a lazy iterator: each step pulls one element from the producer. Exhaust or close() the iterator to release its native handle (garbage collection also releases it).
    """
    _fn = _lib.weaveffi_events_get_messages
    _fn.argtypes = [ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = ctypes.c_void_p
    _err = _WeaveFFIErrorStruct()
    _result = _fn(ctypes.byref(_err))
    _check_error(_err)
    return _GetMessagesIterator(_result)
```

The per-function iterator class implements the Python iterator
protocol. Each `__next__` pulls one element, checks the step's error
slot, and copies the yielded string with `_take_string` (which also
frees the producer's buffer per element):

```python
class _GetMessagesIterator:
    """Lazy iterator over a producer stream: each step pulls one element
    across the C boundary. The native handle is released exactly once, on
    exhaustion, on close(), or when the iterator is garbage collected."""

    def __next__(self):
        if self._done:
            raise StopIteration
        # ... argtypes/restype for _next_fn ...
        _out_item = ctypes.c_void_p()
        _err = _WeaveFFIErrorStruct()
        _has = _next_fn(self._ptr, ctypes.byref(_out_item), ctypes.byref(_err))
        _check_error(_err)
        if not _has:
            self._done = True
            self._destroy()
            raise StopIteration
        return _take_string(_out_item.value)

    def close(self):
        """Release the native iterator without draining it."""
        self._done = True
        self._destroy()
```

The native handle is destroyed exactly once: eagerly on exhaustion,
via `close()` when iteration is abandoned early, or from `__del__` as
a garbage-collection backstop. `_destroy` nulls the stored pointer, so
a double destroy is impossible.

Errors from the launcher and from each `next` follow the function's
error strategy. A throwing iterator such as the `kvstore` sample's
`Store.list_keys` checks each step with `_check_kv_error` and raises
the typed domain error (`KeyNotFound`, `IoError`, ...) from the step
that failed; a non-throwing iterator like `get_messages` raises the
generic `WeaveFFIError` only for producer bugs.

## Troubleshooting

- **`OSError: cannot find ...`**: the loader could not locate the
  shared library. Set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` or copy
  the library next to your script.
- **`WeaveFFIError: ...`**: the Rust side returned a non-zero error
  code. Catch `WeaveFFIError` and inspect `.code` / `.message`.
- **`AttributeError: ... has no attribute 'argtypes'`**: the wrapper
  sets `argtypes`/`restype` at the call site; ensure you're calling
  the generated function, not reaching into `_lib` directly.
- **Garbage-collected struct still referenced from Rust**: keep a
  Python reference until you're done; Python will call `__del__` only
  after the last reference is dropped.
