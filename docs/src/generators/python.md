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
| `Enum` (plain) | `"EnumName"`       | `ctypes.c_int32`                   |
| `Enum` (rich)  | `"EnumName"`       | `ctypes.c_void_p`                  |
| `T?`         | `Optional[T]`        | `ctypes.POINTER(scalar)` for values; same pointer for strings/structs |
| `[T]`        | `List[T]`            | `ctypes.POINTER(scalar)` + `ctypes.c_size_t` |
| `{K: V}`     | `Dict[K, V]`         | key/value pointer arrays + `ctypes.c_size_t` |
| `iter<T>`    | `Iterator[T]`        | opaque `ctypes.c_void_p` iterator handle |

Booleans cross the boundary as `c_int32` (`0`/`1`) because C has no
standard fixed-width boolean type across ABIs.

## Example IDL → generated code

```yaml
version: "0.4.0"
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

Functions become Python functions with full type hints; ctypes
`argtypes`/`restype` are set up at the call site:

```python
def contacts_create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int:
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
        _fn.restype = ctypes.c_char_p
        return _bytes_to_string(_fn(self._ptr)) or ""
```

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

def contacts_create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int: ...
```

Wrapper names carry the IDL module prefix by default
(`contacts_create_contact`); set `strip_module_prefix: true` in the
Python generator config to drop it.

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
from weaveffi import Shape, shapes_describe, shapes_scale

circle = Shape.circle(2.0)
labeled = Shape.labeled("unit", 3)

if circle.tag == Shape.Tag.Circle:
    print(circle.circle_radius)      # 2.0
print(labeled.labeled_count)         # 3

print(shapes_describe(circle))       # render via the C ABI
bigger = shapes_scale(circle, 3.0)   # returns a brand-new Shape
```

**Ownership:** each `Shape` owns its `ctypes.c_void_p`; `__del__` calls
`weaveffi_shapes_Shape_destroy` once the last Python reference is
dropped, and the `Shape` returned by `shapes_scale` is owned the same
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
       contacts_create_contact,
       contacts_get_contact,
       contacts_count_contacts,
   )

   handle = contacts_create_contact("Alice", "alice@example.com", ContactType.Work)
   contact = contacts_get_contact(handle)
   print(f"{contact.name} ({contact.email})")
   print(f"Total: {contacts_count_contacts()}")
   ```

## Memory and ownership

- **Strings in:** Python `str` is encoded to UTF-8 by `_string_to_bytes`
  before crossing the boundary. ctypes manages the lifetime of the
  temporary buffer.
- **Strings out:** Returned `c_char_p` is decoded via
  `_bytes_to_string`. The Rust runtime owns the original pointer; the
  preamble registers `weaveffi_free_string` for cleanup.
- **Bytes:** copied in via a ctypes array, copied out via slicing
  (`_result[:_out_len.value]`). Rust frees the original buffer.
- **Structs:** wrappers hold an opaque `c_void_p`. `__del__` calls the
  matching `_destroy` C function. For deterministic cleanup, use the
  `_PointerGuard` context manager:

  ```python
  with _PointerGuard(handle, _lib.weaveffi_contacts_Contact_destroy):
      ...
  ```

## Async support

Async IDL functions (`async: true`) are exposed as `async def`
wrappers. Each wrapper delegates to a generated blocking
`_<module>_<name>_sync` helper via `run_in_executor`, so the asyncio
event loop stays free while a worker thread waits for the native
completion callback:

```python
async def tasks_run_task(name: str) -> "TaskResult":
    _loop = asyncio.get_event_loop()
    return await _loop.run_in_executor(None, _tasks_run_task_sync, name)
```

The `_sync` helper builds a `ctypes.CFUNCTYPE` completion callback,
calls the `_async`-suffixed C launcher, and blocks on a
`threading.Event` until the C side fires the callback. An error
reported through the callback is re-raised as `WeaveFFIError`:

```python
def _tasks_run_task_sync(name: str) -> "TaskResult":
    _fn = _lib.weaveffi_tasks_run_task_async
    _ev = threading.Event()
    _state = {"err": None, "val": None}
    def _cb_impl(context, err, result):
        try:
            if err and err.contents.code != 0:
                # ... decode the error, weaveffi_error_clear ...
                _state["err"] = WeaveFFIError(_code, _msg)
            else:
                # ... null-pointer guard ...
                _state["val"] = TaskResult(result)
        finally:
            _ev.set()
    _cb_type = ctypes.CFUNCTYPE(None, ctypes.c_void_p,
                                ctypes.POINTER(_WeaveFFIErrorStruct),
                                ctypes.c_void_p)
    _cb = _cb_type(_cb_impl)
    _fn.argtypes = [ctypes.c_char_p, _cb_type, ctypes.c_void_p]
    _fn.restype = None
    _fn(_string_to_bytes(name), _cb, None)
    _ev.wait()
    if _state["err"] is not None:
        raise _state["err"]
    return _state["val"]
```

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter; the Python wrapper always passes `None` (NULL)
for it. The token is not exposed, so cancelling the awaiting asyncio
task does not stop the native operation. Cancellation tokens are
currently surfaced only by the C, C++, and Kotlin targets.

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


def events_register_message_listener(callback: Callable[[str], None]) -> int:
    def _trampoline(message, _context):
        callback(_bytes_to_string(message))
    _cfunc = _CFUNC_weaveffi_events_OnMessage_fn(_trampoline)
    _fn = _lib.weaveffi_events_register_message_listener
    _fn.argtypes = [_CFUNC_weaveffi_events_OnMessage_fn, ctypes.c_void_p]
    _fn.restype = ctypes.c_uint64
    _listener_id = int(_fn(_cfunc, None))
    _listener_refs[_listener_id] = _cfunc
    return _listener_id


def events_unregister_message_listener(listener_id: int) -> None:
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
  to `events_unregister_message_listener` to stop delivery and release
  the trampoline.
- **Threading**: the callback fires on the producer's thread, not the
  thread that registered it. Do not block inside it; if results must
  reach an asyncio loop or UI thread, marshal them yourself (e.g. with
  `loop.call_soon_threadsafe`).

Typical round trip:

```python
listener_id = events_register_message_listener(lambda m: print(m))
events_send_message("hello")
events_unregister_message_listener(listener_id)
```

## Iterators

Functions returning `iter<T>` receive an opaque iterator handle from
the C ABI (`weaveffi_events_get_messages`). The wrapper drains it
eagerly with the generated `_next` binding
(`weaveffi_events_GetMessagesIterator_next`), destroys the handle, and
returns the collected items; the signature is annotated
`Iterator[str]`:

```python
def events_get_messages() -> Iterator[str]:
    _fn = _lib.weaveffi_events_get_messages
    _fn.argtypes = [ctypes.POINTER(_WeaveFFIErrorStruct)]
    _fn.restype = ctypes.c_void_p
    _err = _WeaveFFIErrorStruct()
    _result = _fn(ctypes.byref(_err))
    _check_error(_err)
    # ... argtypes/restype for _next_fn and _destroy_fn ...
    _items = []
    while True:
        _out_item = ctypes.c_char_p()
        _item_err = _WeaveFFIErrorStruct()
        _has = _next_fn(_result, ctypes.byref(_out_item),
                        ctypes.byref(_item_err))
        _check_error(_item_err)
        if not _has:
            break
        _items.append(_bytes_to_string(_out_item.value))
    _destroy_fn(_result)
    return _items
```

An error from `_next` raises `WeaveFFIError`; on success the iterator
handle is destroyed before the wrapper returns, so no native state
outlives the call.

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
