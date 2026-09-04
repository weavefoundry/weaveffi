//! The emitted Python runtime prelude: error base classes, library loading,
//! the ABI-revision check, the runtime symbol bindings (`error_set`,
//! `error_clear`, `error_free`, `free_string`, `free_bytes`), the
//! owned-pointer helpers, the `_BufferWriter`/`_BufferReader` pair
//! implementing the value-buffer wire format (including object tokens), and
//! the feature-gated async and callback-interface registries.

use weaveffi_core::cabi::ABI_VERSION;

/// Emit the load-time ABI-revision check that runs right after `_lib` is
/// opened and before any other symbol is bound. A missing symbol means the
/// producer predates ABI versioning; a different value means it was built
/// against an incompatible runtime. Both raise `ImportError` so the failure
/// surfaces at `import` time with a message naming both revisions.
fn render_abi_version_check(out: &mut String) {
    out.push_str(&format!(
        r#"
# The ABI revision these bindings were generated against. Checked before any
# other symbol is bound so a mismatched producer fails at import time instead
# of misreading the error struct or a value buffer later.
_ABI_VERSION = {ABI_VERSION}


def _check_abi_version(lib: ctypes.CDLL) -> None:
    try:
        fn = lib.weaveffi_abi_version
    except AttributeError:
        raise ImportError(
            "the loaded WeaveFFI library predates ABI versioning "
            f"(these bindings expect ABI revision {{_ABI_VERSION}})"
        ) from None
    fn.argtypes = []
    fn.restype = ctypes.c_uint32
    found = fn()
    if found != _ABI_VERSION:
        raise ImportError(
            f"WeaveFFI ABI mismatch: these bindings expect revision {{_ABI_VERSION}} "
            f"but the loaded library reports revision {{found}}"
        )


_check_abi_version(_lib)
"#
    ));
}

/// Append the fixed runtime prelude every generated `weaveffi.py` starts
/// with.
pub(crate) fn render_preamble(out: &mut String) {
    out.push_str(
        r#""""WeaveFFI Python ctypes bindings (auto-generated)"""
import contextlib
import ctypes
import os
import platform
import struct
from dataclasses import dataclass
from enum import IntEnum
from typing import Callable, Dict, Iterator, List, Optional


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


class _WeaveFFIErrorStruct(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
        ("payload_ptr", ctypes.c_void_p),
        ("payload_len", ctypes.c_size_t),
    ]


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
"#,
    );
    render_abi_version_check(out);
    out.push_str(
        r#"# Callback-interface trampolines report a failed implementation through
# weaveffi_error_set, which copies the message with the producer's allocator.
_lib.weaveffi_error_set.argtypes = [
    ctypes.POINTER(_WeaveFFIErrorStruct),
    ctypes.c_int32,
    ctypes.c_char_p,
]
_lib.weaveffi_error_set.restype = None
_lib.weaveffi_error_clear.argtypes = [ctypes.POINTER(_WeaveFFIErrorStruct)]
_lib.weaveffi_error_clear.restype = None
# Async completion callbacks receive a heap-boxed error the consumer owns;
# weaveffi_error_free releases the message, the payload, and the box.
_lib.weaveffi_error_free.argtypes = [ctypes.POINTER(_WeaveFFIErrorStruct)]
_lib.weaveffi_error_free.restype = None
# The free helpers take raw addresses (`c_void_p`) so wrappers can release
# owned producer allocations they hold as plain integers or typed pointers.
_lib.weaveffi_free_string.argtypes = [ctypes.c_void_p]
_lib.weaveffi_free_string.restype = None
_lib.weaveffi_free_bytes.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
_lib.weaveffi_free_bytes.restype = None


def _check_error(err: _WeaveFFIErrorStruct) -> None:
    if err.code != 0:
        code = err.code
        message = err.message.decode("utf-8") if err.message else ""
        _lib.weaveffi_error_clear(ctypes.byref(err))
        raise WeaveFFIError(code, message)


class _PointerGuard(contextlib.AbstractContextManager):
    def __init__(self, ptr, free_fn) -> None:
        self.ptr = ptr
        self._free_fn = free_fn

    def __exit__(self, *exc) -> bool:
        if self.ptr is not None:
            self._free_fn(self.ptr)
            self.ptr = None
        return False


def _string_to_bytes(s: Optional[str]) -> Optional[bytes]:
    if s is None:
        return None
    return s.encode("utf-8")


def _bytes_to_string(ptr) -> Optional[str]:
    if ptr is None:
        return None
    return ptr.decode("utf-8")


def _take_string(ptr) -> Optional[str]:
    """Copy an owned C string (a raw address) and free the producer buffer."""
    if not ptr:
        return None
    _s = ctypes.string_at(ptr).decode("utf-8")
    _lib.weaveffi_free_string(ptr)
    return _s


def _borrow(obj):
    """The raw pointer of a live object wrapper, lent to the producer for the
    duration of one call. Raises if the wrapper was already closed."""
    _p = obj._ptr
    if _p is None:
        raise WeaveFFIError(-1, f"{type(obj).__name__} used after close()")
    return _p


class _BufferWriter:
    """Encodes values into the WeaveFFI value-buffer wire format:
    little-endian, packed, no alignment."""

    def __init__(self) -> None:
        self._buf = bytearray()
        self._objects = []

    def finish(self) -> bytes:
        # Mint the object tokens last: an encoding that raised part-way
        # (a closed wrapper, a field of the wrong type) never reaches here,
        # so it leaks no strong references.
        for _pos, _obj in self._objects:
            struct.pack_into("<Q", self._buf, _pos, _obj._clone_ptr())
        self._objects = []
        return bytes(self._buf)

    def write_bool(self, v) -> None:
        self._buf.append(1 if v else 0)

    def write_i8(self, v: int) -> None:
        self._buf += struct.pack("<b", v)

    def write_u8(self, v: int) -> None:
        self._buf += struct.pack("<B", v)

    def write_i16(self, v: int) -> None:
        self._buf += struct.pack("<h", v)

    def write_u16(self, v: int) -> None:
        self._buf += struct.pack("<H", v)

    def write_i32(self, v: int) -> None:
        self._buf += struct.pack("<i", v)

    def write_u32(self, v: int) -> None:
        self._buf += struct.pack("<I", v)

    def write_i64(self, v: int) -> None:
        self._buf += struct.pack("<q", v)

    def write_u64(self, v: int) -> None:
        self._buf += struct.pack("<Q", v)

    def write_f32(self, v: float) -> None:
        self._buf += struct.pack("<f", v)

    def write_f64(self, v: float) -> None:
        self._buf += struct.pack("<d", v)

    def write_len(self, n: int) -> None:
        self._buf += struct.pack("<I", n)

    def write_option_flag(self, present) -> None:
        self._buf.append(1 if present else 0)

    def write_string(self, v: str) -> None:
        _b = v.encode("utf-8")
        self.write_len(len(_b))
        self._buf += _b

    def write_bytes(self, v: bytes) -> None:
        self.write_len(len(v))
        self._buf += bytes(v)

    def write_object(self, obj) -> None:
        """Write an object token: one strong reference to `obj` the reader
        will adopt. The wrapper is checked to be live now (a closed one
        raises), but the reference itself is cloned in finish()."""
        _borrow(obj)
        self._objects.append((len(self._buf), obj))
        self._buf += bytes(8)


class _BufferReader:
    """Decodes values from the WeaveFFI value-buffer wire format, rejecting
    truncated buffers, invalid flag bytes, invalid UTF-8, and oversized
    length prefixes."""

    def __init__(self, data: bytes) -> None:
        self._data = memoryview(data)
        self._pos = 0

    def _take(self, n: int, what: str) -> memoryview:
        if len(self._data) - self._pos < n:
            raise WeaveFFIError(-1, f"malformed value buffer: truncated {what}")
        _view = self._data[self._pos:self._pos + n]
        self._pos += n
        return _view

    def read_bool(self) -> bool:
        _b = self._take(1, "bool")[0]
        if _b > 1:
            raise WeaveFFIError(-1, "malformed value buffer: invalid bool byte")
        return _b == 1

    def read_i8(self) -> int:
        return struct.unpack("<b", self._take(1, "i8"))[0]

    def read_u8(self) -> int:
        return struct.unpack("<B", self._take(1, "u8"))[0]

    def read_i16(self) -> int:
        return struct.unpack("<h", self._take(2, "i16"))[0]

    def read_u16(self) -> int:
        return struct.unpack("<H", self._take(2, "u16"))[0]

    def read_i32(self) -> int:
        return struct.unpack("<i", self._take(4, "i32"))[0]

    def read_u32(self) -> int:
        return struct.unpack("<I", self._take(4, "u32"))[0]

    def read_i64(self) -> int:
        return struct.unpack("<q", self._take(8, "i64"))[0]

    def read_u64(self) -> int:
        return struct.unpack("<Q", self._take(8, "u64"))[0]

    def read_f32(self) -> float:
        return struct.unpack("<f", self._take(4, "f32"))[0]

    def read_f64(self) -> float:
        return struct.unpack("<d", self._take(8, "f64"))[0]

    def read_len(self) -> int:
        _n = struct.unpack("<I", self._take(4, "length prefix"))[0]
        if _n > len(self._data) - self._pos:
            raise WeaveFFIError(
                -1, "malformed value buffer: length prefix exceeds remaining bytes"
            )
        return _n

    def read_option_flag(self) -> bool:
        _b = self._take(1, "option flag")[0]
        if _b > 1:
            raise WeaveFFIError(-1, "malformed value buffer: invalid option flag")
        return _b == 1

    def read_string(self) -> str:
        _n = self.read_len()
        try:
            return str(self._take(_n, "string data"), "utf-8")
        except UnicodeDecodeError as _e:
            raise WeaveFFIError(
                -1, "malformed value buffer: string is not valid UTF-8"
            ) from _e

    def read_bytes(self) -> bytes:
        _n = self.read_len()
        return bytes(self._take(_n, "bytes data"))

    def read_object(self) -> int:
        """Read an object token: one strong reference the caller adopts into
        a wrapper. Zero never appears in a non-optional position."""
        _p = struct.unpack("<Q", self._take(8, "object token"))[0]
        if _p == 0:
            raise WeaveFFIError(-1, "malformed value buffer: null object token")
        return _p

    def expect_end(self) -> None:
        if self._pos != len(self._data):
            raise WeaveFFIError(-1, "malformed value buffer: trailing bytes")


def _decode_buffer(data: bytes, read_fn):
    """Decode exactly one value from `data` using `read_fn(reader)`."""
    _r = _BufferReader(data)
    _v = read_fn(_r)
    _r.expect_end()
    return _v


def _take_buffer(ptr, length) -> bytes:
    """Copy an owned value buffer (a raw address) and release it with
    weaveffi_free_bytes."""
    if not ptr:
        return b""
    _data = ctypes.string_at(ptr, length) if length else b""
    _lib.weaveffi_free_bytes(ptr, ctypes.c_size_t(length))
    return _data
"#,
    );
}

/// Append the feature-gated runtime pieces after the fixed preamble: the
/// `asyncio` completion registry when the API has async callables, and the
/// callback-interface handle table (plus the `abc` import the abstract base
/// classes need) when it declares callback interfaces. Both registries share
/// one `threading` import.
pub(crate) fn render_feature_runtime(out: &mut String, has_async: bool, has_callbacks: bool) {
    if !has_async && !has_callbacks {
        return;
    }
    out.push('\n');
    if has_callbacks {
        out.push_str("import abc\n");
    }
    if has_async {
        out.push_str("import asyncio\n");
    }
    out.push_str("import threading\n");
    if has_async {
        out.push_str(
            r#"

# Pending async completion trampolines, keyed by an integer token.
# Holding the ctypes function objects here keeps them alive until the
# producer fires the completion callback, even when the awaiting
# coroutine has been cancelled; each entry is removed on completion.
_async_pending: Dict[int, object] = {}
_async_lock = threading.Lock()
_async_next_token = 0


def _async_register(cb) -> int:
    global _async_next_token
    with _async_lock:
        _async_next_token += 1
        _token = _async_next_token
        _async_pending[_token] = cb
    return _token
"#,
        );
    }
    if has_callbacks {
        out.push_str(
            r#"

# Live callback-interface implementations, keyed by the integer the producer
# receives as `ctx`. An entry is added when an implementation is passed to a
# call and removed when the producer invokes the vtable's `free(ctx)`, so the
# implementation stays alive exactly as long as the producer may call it and
# the producer never holds a raw reference to a Python object.
_cb_impls: Dict[int, object] = {}
_cb_lock = threading.Lock()
_cb_next_ctx = 0


def _cb_register(impl) -> int:
    global _cb_next_ctx
    with _cb_lock:
        _cb_next_ctx += 1
        _ctx = _cb_next_ctx
        _cb_impls[_ctx] = impl
    return _ctx
"#,
        );
    }
}

/// The exact `_load_library` block [`render_preamble`] emits in `generate`
/// mode, so the packager can swap it for a bundled-first variant.
pub(crate) const PY_LOADER_ORIGINAL: &str = r#"def _load_library() -> ctypes.CDLL:
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
    return ctypes.CDLL(name)"#;

/// The packaged `_load_library` for `lib`: prefer the per-platform library
/// bundled next to the module, then `WEAVEFFI_LIBRARY`, then the system path.
pub(crate) fn py_loader_packaged(lib: &str) -> String {
    format!(
        r#"def _load_library() -> ctypes.CDLL:
    # A bundled per-platform library ships next to this module; prefer it so the
    # package works with no external setup. WEAVEFFI_LIBRARY still overrides.
    override = os.environ.get("WEAVEFFI_LIBRARY")
    if override:
        return ctypes.CDLL(override)
    here = os.path.dirname(os.path.abspath(__file__))
    system = platform.system()
    if system == "Darwin":
        name = "lib{lib}.dylib"
    elif system == "Windows":
        name = "{lib}.dll"
    else:
        name = "lib{lib}.so"
    bundled = os.path.join(here, name)
    if os.path.exists(bundled):
        return ctypes.CDLL(bundled)
    return ctypes.CDLL(name)"#
    )
}
