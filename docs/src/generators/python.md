# Python

The Python generator produces pure-Python ctypes bindings, type stubs, and
packaging files. It uses Python's built-in `ctypes` module to call the C ABI
directly — no compilation step, no native extension modules, no third-party
dependencies.

## Why ctypes?

- **Zero dependencies.** `ctypes` ships with every CPython install since Python 2.5.
- **Works with any Python 3.7+.** No version-specific native extensions to maintain.
- **No build step.** The generated `.py` files are plain Python — `pip install .`
  is enough.
- **Transparent.** Developers can read and debug the generated code directly.

The trade-off is that ctypes calls are slower than compiled extensions (cffi,
pybind11, PyO3). For most FFI workloads the overhead is negligible compared to
the work done inside the Rust library.

## Generated artifacts

| File | Purpose |
|------|---------|
| `python/weaveffi/__init__.py` | Re-exports everything from `weaveffi.py` |
| `python/weaveffi/weaveffi.py` | ctypes bindings: library loader, wrapper functions, classes |
| `python/weaveffi/weaveffi.pyi` | Type stub for IDE autocompletion and mypy |
| `python/pyproject.toml` | PEP 621 project metadata |
| `python/setup.py` | Fallback setuptools script |
| `python/README.md` | Basic usage instructions |

## Generated code examples

Given this IDL definition:

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

### Library loader

The generated module auto-detects the platform and loads the shared library:

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

### Functions

Each IDL function becomes a Python function with full type hints. The wrapper
declares ctypes argtypes/restype, converts arguments, calls the C symbol, checks
for errors, and converts the return value:

```python
def create_contact(name: str, email: Optional[str], contact_type: "ContactType") -> int:
    _fn = _lib.weaveffi_contacts_create_contact
    _fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int32, ctypes.POINTER(_WeaveffiErrorStruct)]
    _fn.restype = ctypes.c_uint64
    _email_c = _string_to_bytes(email)
    _err = _WeaveffiErrorStruct()
    _result = _fn(_string_to_bytes(name), _email_c, contact_type.value, ctypes.byref(_err))
    _check_error(_err)
    return _result
```

### Enums

Enums map to Python `IntEnum` subclasses:

```python
class ContactType(IntEnum):
    """Type of contact"""
    Personal = 0
    Work = 1
    Other = 2
```

Enum parameters are passed as `.value` (an `int32`); enum returns are wrapped
back into the enum class.

### Structs

Structs become Python classes backed by an opaque pointer. Fields are exposed
as `@property` getters that call the corresponding C getter function:

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
        _result = _fn(self._ptr)
        return _bytes_to_string(_result) or ""

    @property
    def email(self) -> Optional[str]:
        _fn = _lib.weaveffi_contacts_Contact_get_email
        _fn.argtypes = [ctypes.c_void_p]
        _fn.restype = ctypes.c_char_p
        _result = _fn(self._ptr)
        return _bytes_to_string(_result)

    @property
    def age(self) -> int:
        _fn = _lib.weaveffi_contacts_Contact_get_age
        _fn.argtypes = [ctypes.c_void_p]
        _fn.restype = ctypes.c_int32
        _result = _fn(self._ptr)
        return _result
```

### Type stubs (.pyi)

The generator also produces a `.pyi` stub file for IDE support and static
analysis tools like mypy:

```python
from enum import IntEnum
from typing import Dict, List, Optional

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
def get_contact(id: int) -> "Contact": ...
def count_contacts() -> int: ...
```

## Type mapping reference

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

Booleans are transmitted as `c_int32` (`0`/`1`) because C has no standard
fixed-width boolean type across ABIs.

## Build and install

### 1. Generate bindings

```bash
weaveffi generate --input api.yaml --output generated/ --target python
```

### 2. Build the Rust shared library

```bash
cargo build --release -p your_library
```

This produces `libweaveffi.dylib` (macOS), `libweaveffi.so` (Linux), or
`weaveffi.dll` (Windows) in `target/release/`.

### 3. Install the Python package

```bash
cd generated/python
pip install .
```

Or for development:

```bash
pip install -e .
```

### 4. Make the shared library findable

The shared library must be on the system library search path at runtime:

**macOS:**
```bash
DYLD_LIBRARY_PATH=../../target/release python your_script.py
```

**Linux:**
```bash
LD_LIBRARY_PATH=../../target/release python your_script.py
```

**Windows:**
Place `weaveffi.dll` in the same directory as your script, or add its
directory to `PATH`.

### 5. Use the bindings

```python
from weaveffi import ContactType, create_contact, get_contact, count_contacts

handle = create_contact("Alice", "alice@example.com", ContactType.Work)
contact = get_contact(handle)
print(f"{contact.name} ({contact.email})")
print(f"Total: {count_contacts()}")
```

## Memory management

The generated Python wrappers handle memory ownership automatically:

### Strings

- **Passing strings in:** Python `str` values are encoded to UTF-8 bytes via
  `_string_to_bytes()` before crossing the FFI boundary. ctypes manages the
  lifetime of these temporary byte buffers.
- **Receiving strings back:** Returned `c_char_p` values are decoded from
  UTF-8 via `_bytes_to_string()`. The Rust runtime owns the returned pointer;
  the preamble registers `weaveffi_free_string` for cleanup.

### Bytes

- **Passing bytes in:** Python `bytes` are copied into a ctypes array
  (`(c_uint8 * len(data))(*data)`) and passed with a length parameter.
- **Receiving bytes back:** The C function writes to an `out_len` parameter.
  The wrapper copies the data into a Python `bytes` object via slicing
  (`_result[:_out_len.value]`), then the Rust side is responsible for the
  original buffer.

### Structs (opaque pointers)

Struct wrappers hold an opaque `c_void_p`. The `__del__` destructor calls the
corresponding `_destroy` C function to free the Rust-side allocation:

```python
def __del__(self) -> None:
    if self._ptr is not None:
        _lib.weaveffi_contacts_Contact_destroy(self._ptr)
        self._ptr = None
```

The `_PointerGuard` context manager is available for explicit lifetime control:

```python
class _PointerGuard(contextlib.AbstractContextManager):
    def __init__(self, ptr, free_fn) -> None:
        self.ptr = ptr
        self._free_fn = free_fn

    def __exit__(self, *exc) -> bool:
        if self.ptr is not None:
            self._free_fn(self.ptr)
            self.ptr = None
        return False
```

## Error handling

C-level errors are converted to Python exceptions automatically. The generated
module defines a `WeaveffiError` exception class:

```python
class WeaveffiError(Exception):
    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"({code}) {message}")
```

Every function call follows this pattern:

1. A `_WeaveffiErrorStruct` (mirroring the C `weaveffi_error`) is allocated.
2. It is passed as the last argument to the C function via `ctypes.byref()`.
3. After the call, `_check_error()` inspects the struct. If `code != 0`, it
   reads the message, calls `weaveffi_error_clear` to free the Rust-allocated
   string, and raises `WeaveffiError`.

```python
class _WeaveffiErrorStruct(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
    ]

def _check_error(err: _WeaveffiErrorStruct) -> None:
    if err.code != 0:
        code = err.code
        message = err.message.decode("utf-8") if err.message else ""
        _lib.weaveffi_error_clear(ctypes.byref(err))
        raise WeaveffiError(code, message)
```

Callers use standard Python `try`/`except`:

```python
from weaveffi import create_contact, ContactType, WeaveffiError

try:
    handle = create_contact("Alice", None, ContactType.Personal)
except WeaveffiError as e:
    print(f"Error {e.code}: {e.message}")
```
