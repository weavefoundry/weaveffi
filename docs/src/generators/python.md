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

## Type mapping

| IDL type     | Python type hint     | ctypes type                        |
|--------------|----------------------|------------------------------------|
| `i32`        | `int`                | `ctypes.c_int32`                   |
| `u32`        | `int`                | `ctypes.c_uint32`                  |
| `i64`        | `int`                | `ctypes.c_int64`                   |
| `f64`        | `float`              | `ctypes.c_double`                  |
| `bool`       | `bool`               | `ctypes.c_int32`                   |
| `string`     | `str`                | `ctypes.c_char_p`                  |
| `bytes`      | `bytes`              | `ctypes.POINTER(ctypes.c_uint8)` + `ctypes.c_size_t` |
| `handle`     | `int`                | `ctypes.c_uint64`                  |
| `Struct`     | `"StructName"`       | `ctypes.c_void_p`                  |
| `Enum`       | `"EnumName"`         | `ctypes.c_int32`                   |
| `T?`         | `Optional[T]`        | `ctypes.POINTER(scalar)` for values; same pointer for strings/structs |
| `[T]`        | `List[T]`            | `ctypes.POINTER(scalar)` + `ctypes.c_size_t` |
| `{K: V}`     | `Dict[K, V]`         | key/value pointer arrays + `ctypes.c_size_t` |

Booleans cross the boundary as `c_int32` (`0`/`1`) because C has no
standard fixed-width boolean type across ABIs.

## Example IDL → generated code

```yaml
version: "0.3.0"
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
def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int:
    _fn = _lib.weaveffi_contacts_create_contact
    _fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int32,
                    ctypes.POINTER(_WeaveffiErrorStruct)]
    _fn.restype = ctypes.c_uint64
    _err = _WeaveffiErrorStruct()
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

def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int: ...
```

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate --input api.yaml --output generated/ --target python
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
   from weaveffi import ContactType, create_contact, get_contact, count_contacts

   handle = create_contact("Alice", "alice@example.com", ContactType.Work)
   contact = get_contact(handle)
   print(f"{contact.name} ({contact.email})")
   print(f"Total: {count_contacts()}")
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

Async IDL functions are exposed as `async def` wrappers that schedule
the C ABI callback onto the running asyncio event loop using
`loop.call_soon_threadsafe` and a `Future`. The wrapper captures the
loop, hands the C ABI a callback that resolves the future, and awaits
it:

```python
async def fetch_contact(id: int) -> Contact:
    loop = asyncio.get_running_loop()
    fut: asyncio.Future[Contact] = loop.create_future()
    _ctx_id = _retain_ctx((loop, fut))
    _lib.weaveffi_contacts_fetch_contact_async(id, _async_trampoline, _ctx_id)
    return await fut
```

When the IDL marks the function `cancel: true`, the wrapper hooks the
asyncio cancellation into a `weaveffi_cancel_token`.

## Troubleshooting

- **`OSError: cannot find ...`** — the loader could not locate the
  shared library. Set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` or copy
  the library next to your script.
- **`WeaveffiError: ...`** — the Rust side returned a non-zero error
  code. Catch `WeaveffiError` and inspect `.code` / `.message`.
- **`AttributeError: ... has no attribute 'argtypes'`** — the wrapper
  sets `argtypes`/`restype` at the call site; ensure you're calling
  the generated function, not reaching into `_lib` directly.
- **Garbage-collected struct still referenced from Rust** — keep a
  Python reference until you're done; Python will call `__del__` only
  after the last reference is dropped.
