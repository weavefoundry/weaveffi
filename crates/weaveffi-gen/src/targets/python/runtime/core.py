"""ctypes bindings for the {{NAME}} native library."""
{{IMPORTS}}import builtins
import ctypes
import os
import platform
import struct
import threading
import warnings
from dataclasses import dataclass
from enum import IntEnum
from typing import Any, Callable, Dict, Iterator, List, Optional, Tuple, Type


class {{ERROR}}(Exception):
    """An error reported by the native library.

    `code` is positive for one of the API's declared error codes (raised as
    the declaring module's typed subclasses) and negative for a runtime trap:
    a generic failure (-1), a producer panic (-2), a marshalling failure
    (-3), or an exception raised by a callback-interface implementation
    (-4). A cancelled async call (-5) raises `asyncio.CancelledError`
    instead.
    """

    GENERIC_ERROR_CODE = -1
    PANIC_ERROR_CODE = -2
    MARSHAL_ERROR_CODE = -3
    FOREIGN_ERROR_CODE = -4
    CANCELLED_ERROR_CODE = -5

    def __init__(self, code: int, message: str) -> None:
        super().__init__(f"({code}) {message}")
        self.code = code
        self.message = message


class _ErrorStruct(ctypes.Structure):
    """The C `{{PREFIX}}_error` out-parameter every call reports through."""

    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
        ("payload_ptr", ctypes.c_void_p),
        ("payload_len", ctypes.c_size_t),
    ]


def _load_library() -> ctypes.CDLL:
    # An explicit path wins; then a library bundled inside this package (a
    # platform wheel); then the system loader's search path.
    override = os.environ.get("{{LIBRARY_ENV}}")
    if override:
        return ctypes.CDLL(override)
    system = platform.system()
    if system == "Darwin":
        name = "{{LIB_DARWIN}}"
    elif system == "Windows":
        name = "{{LIB_WINDOWS}}"
    else:
        name = "{{LIB_LINUX}}"
    bundled = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    if os.path.exists(bundled):
        return ctypes.CDLL(bundled)
    try:
        return ctypes.CDLL(name)
    except OSError as exc:
        raise ImportError(
            f"cannot load the native library {name!r} ({exc}); "
            "set {{LIBRARY_ENV}} to its path"
        ) from exc


_lib = _load_library()


def _bind(name: str, restype: Any, *argtypes: Any) -> Any:
    """Look up one exported symbol and fix its C signature. Every prototype
    is bound once, at import time."""
    fn = getattr(_lib, name)
    fn.restype = restype
    fn.argtypes = argtypes
    return fn


# The C ABI revision these bindings were generated against, and the contract
# checksum of every top-level module: (module, checksum symbol, value).
_ABI_VERSION = {{ABI_VERSION}}
_CHECKSUMS: List[Tuple[str, str, int]] = [
{{CHECKSUMS}}
]


def _check_contract() -> None:
    """Refuse to load a library built for a different ABI revision or a
    different version of any module's API, before any other symbol is
    bound."""
    try:
        abi_version = _bind("{{PREFIX}}_abi_version", ctypes.c_uint32)
    except AttributeError:
        raise ImportError(
            f"{_lib._name} does not export {{PREFIX}}_abi_version; "
            "it is not the library these bindings were generated for"
        ) from None
    found = abi_version()
    if found != _ABI_VERSION:
        raise ImportError(
            f"C ABI mismatch: these bindings expect revision {_ABI_VERSION}, "
            f"but {_lib._name} reports revision {found}"
        )
    for module, symbol, expected in _CHECKSUMS:
        try:
            checksum = _bind(symbol, ctypes.c_uint64)
        except AttributeError:
            raise ImportError(
                f"module {module!r}: {_lib._name} does not export {symbol}"
            ) from None
        found = checksum()
        if found != expected:
            raise ImportError(
                f"module {module!r}: the native library's contract checksum "
                f"{found:#018x} does not match these bindings ({expected:#018x}); "
                "regenerate the bindings from the library's current API"
            )


_check_contract()

_error_clear = _bind("{{PREFIX}}_error_clear", None, ctypes.POINTER(_ErrorStruct))
_free_bytes = _bind("{{PREFIX}}_free_bytes", None, ctypes.c_void_p, ctypes.c_size_t)


def _debug_live(kind: int) -> int:
    """The producer's live-allocation counter `kind` (0 objects, 1 callbacks,
    2 iterators, 3 cancel tokens, 4 returned allocations). Always 0 unless
    the producer was built with its leak counters enabled."""
    return _bind("{{PREFIX}}_debug_live", ctypes.c_uint64, ctypes.c_int32)(kind)


def _read_error(err: _ErrorStruct) -> Tuple[int, str, bytes]:
    """Copy a filled out-err slot's code, message, and payload, then release
    them."""
    code = err.code
    message = err.message.decode("utf-8", "replace") if err.message else ""
    payload = ctypes.string_at(err.payload_ptr, err.payload_len) if err.payload_ptr else b""
    _error_clear(ctypes.byref(err))
    return code, message, payload


def _error_from(code: int, message: str, payload: bytes = b"") -> {{ERROR}}:
    """The exception for a code no declared error domain claims."""
    return {{ERROR}}(code, message)


def _check_error(err: _ErrorStruct) -> None:
    """Raise for a non-zero out-err slot of a call that declares no errors:
    only runtime traps (a panic, a marshalling failure, a failed callback)
    land here."""
    if err.code:
        raise _error_from(*_read_error(err))


def _take_bytes(ptr: Optional[int], length: int) -> bytes:
    """Copy a producer-owned byte run (a string, bytes, or value buffer) and
    release it with {{PREFIX}}_free_bytes."""
    if not ptr:
        return b""
    try:
        return ctypes.string_at(ptr, length)
    finally:
        _free_bytes(ptr, length)


def _take_str(ptr: Optional[int], length: int) -> str:
    """Copy and release a producer-owned UTF-8 string."""
    return _take_bytes(ptr, length).decode("utf-8")


def _peek_bytes(ptr: Optional[int], length: int) -> bytes:
    """Copy a byte run the producer only lends for the current call."""
    return ctypes.string_at(ptr, length) if ptr and length else b""


class _Handle:
    """Owns one native reference and releases it exactly once.

    Calls lend the pointer through `_acquire`/`_release`, so `close()` from
    another thread while a call is in flight defers the release until the
    last such call returns instead of freeing the object mid-call.
    """

    # The native destroy (or iterator release) function, set per subclass.
    _destroy: Callable[[int], None]

    def _init_handle(self, ptr: int) -> None:
        self._ptr: Optional[int] = ptr
        self._calls = 0
        self._closing = False
        self._lock = threading.Lock()

    @classmethod
    def _adopt(cls, ptr: int) -> Any:
        """Wrap one strong reference the producer handed over."""
        obj = cls.__new__(cls)
        obj._init_handle(ptr)
        return obj

    def _acquire(self) -> int:
        with self._lock:
            ptr = self._ptr
            if ptr is None or self._closing:
                raise {{ERROR}}(-1, f"{type(self).__name__} used after close()")
            self._calls += 1
            return ptr

    def _release(self) -> None:
        with self._lock:
            self._calls -= 1
            if self._calls or not self._closing:
                return
            ptr, self._ptr = self._ptr, None
        if ptr is not None:
            type(self)._destroy(ptr)

    def close(self) -> None:
        """Release this wrapper's native reference. Idempotent; the wrapper
        is unusable afterwards."""
        lock = getattr(self, "_lock", None)
        if lock is None:
            return
        with lock:
            if self._closing:
                return
            self._closing = True
            if self._calls:
                return
            ptr, self._ptr = self._ptr, None
        if ptr is not None:
            type(self)._destroy(ptr)

    def __enter__(self) -> Any:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        # An unreachable wrapper has no call in flight: release directly.
        ptr = getattr(self, "_ptr", None)
        if ptr is not None:
            self._ptr = None
            type(self)._destroy(ptr)


class _Object(_Handle):
    """Base of every interface wrapper: a reference-counted native object."""

    # The native clone function, set per subclass.
    _clone: Callable[[int], int]

    def _clone_ref(self) -> int:
        """A new strong reference to the same object, for a receiver that
        adopts it (an object token inside a value buffer)."""
        ptr = self._acquire()
        try:
            return type(self)._clone(ptr)
        finally:
            self._release()


class _Iterator(_Handle):
    """Base of every lazy iterator over a producer stream: one native `next`
    call per step; the handle is released on exhaustion, `close()`, or
    garbage collection."""

    def __iter__(self) -> Any:
        return self

    def _step(self) -> int:
        with self._lock:
            ptr = self._ptr
            if ptr is None or self._closing:
                raise StopIteration
            self._calls += 1
            return ptr


def _required(ptr: Optional[int]) -> int:
    """A non-null object pointer the producer returned."""
    if not ptr:
        raise {{ERROR}}(-1, "the producer returned a null object")
    return ptr


def _lend(obj: Any, cls: "Type[_Object]") -> int:
    """Lend `obj`'s native pointer to one call; pair with `obj._release()`."""
    if not isinstance(obj, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(obj).__name__}")
    return obj._acquire()


def _lend_opt(obj: Any, cls: "Type[_Object]") -> Optional[int]:
    """`_lend` for an optional object: None lends a null pointer."""
    return None if obj is None else _lend(obj, cls)


def _release_opt(obj: Any) -> None:
    if obj is not None:
        obj._release()


_I8 = struct.Struct("<b")
_U8 = struct.Struct("<B")
_I16 = struct.Struct("<h")
_U16 = struct.Struct("<H")
_I32 = struct.Struct("<i")
_U32 = struct.Struct("<I")
_I64 = struct.Struct("<q")
_U64 = struct.Struct("<Q")
_F32 = struct.Struct("<f")
_F64 = struct.Struct("<d")


def _malformed(what: str) -> {{ERROR}}:
    return {{ERROR}}(-3, f"malformed value buffer: {what}")


class _Writer:
    """Encodes values in the value-buffer wire format: little-endian,
    packed, no alignment."""

    __slots__ = ("_buf", "_objects")

    def __init__(self) -> None:
        self._buf = bytearray()
        self._objects: List[Tuple[int, _Object]] = []

    def finish(self) -> bytes:
        # Object tokens are minted last, so an encoding that fails part-way
        # never leaks a strong reference.
        minted: List[Tuple[_Object, int]] = []
        try:
            for pos, obj in self._objects:
                ptr = obj._clone_ref()
                minted.append((obj, ptr))
                _U64.pack_into(self._buf, pos, ptr)
        except BaseException:
            for obj, ptr in minted:
                type(obj)._destroy(ptr)
            raise
        self._objects = []
        return bytes(self._buf)

    def write_bool(self, v: bool) -> None:
        self._buf.append(1 if v else 0)

    def write_i8(self, v: int) -> None:
        self._buf += _I8.pack(v)

    def write_u8(self, v: int) -> None:
        self._buf += _U8.pack(v)

    def write_i16(self, v: int) -> None:
        self._buf += _I16.pack(v)

    def write_u16(self, v: int) -> None:
        self._buf += _U16.pack(v)

    def write_i32(self, v: int) -> None:
        self._buf += _I32.pack(v)

    def write_u32(self, v: int) -> None:
        self._buf += _U32.pack(v)

    def write_i64(self, v: int) -> None:
        self._buf += _I64.pack(v)

    def write_u64(self, v: int) -> None:
        self._buf += _U64.pack(v)

    def write_f32(self, v: float) -> None:
        self._buf += _F32.pack(v)

    def write_f64(self, v: float) -> None:
        self._buf += _F64.pack(v)

    def write_count(self, n: int) -> None:
        self._buf += _U32.pack(n)

    def write_flag(self, present: bool) -> None:
        self._buf.append(1 if present else 0)

    def write_string(self, v: str) -> None:
        data = v.encode("utf-8")
        self._buf += _U32.pack(len(data))
        self._buf += data

    def write_bytes(self, v: bytes) -> None:
        self._buf += _U32.pack(len(v))
        self._buf += v

    def write_numbers(self, code: str, values: List[Any]) -> None:
        """A list of one fixed-width numeric primitive (`code` is its struct
        format character), packed in one call."""
        self._buf += _U32.pack(len(values))
        self._buf += struct.pack(f"<{len(values)}{code}", *values)

    def write_object(self, obj: Any, cls: "Type[_Object]") -> None:
        """Reserve an object token: one strong reference to `obj` the reader
        adopts, cloned in `finish()`. A closed wrapper is rejected now."""
        if not isinstance(obj, cls):
            raise TypeError(f"expected {cls.__name__}, got {type(obj).__name__}")
        obj._acquire()
        obj._release()
        self._objects.append((len(self._buf), obj))
        self._buf += bytes(8)


class _Reader:
    """Decodes values from the value-buffer wire format, rejecting truncated
    buffers, invalid flag bytes, invalid UTF-8, and trailing data."""

    __slots__ = ("_data", "_pos")

    def __init__(self, data: bytes) -> None:
        self._data = data
        self._pos = 0

    def _take(self, n: int, what: str) -> int:
        pos = self._pos
        if len(self._data) - pos < n:
            raise _malformed(f"truncated {what}")
        self._pos = pos + n
        return pos

    def read_bool(self) -> bool:
        b = self._data[self._take(1, "bool")]
        if b > 1:
            raise _malformed("invalid bool byte")
        return b == 1

    def read_i8(self) -> int:
        return int(_I8.unpack_from(self._data, self._take(1, "i8"))[0])

    def read_u8(self) -> int:
        return self._data[self._take(1, "u8")]

    def read_i16(self) -> int:
        return int(_I16.unpack_from(self._data, self._take(2, "i16"))[0])

    def read_u16(self) -> int:
        return int(_U16.unpack_from(self._data, self._take(2, "u16"))[0])

    def read_i32(self) -> int:
        return int(_I32.unpack_from(self._data, self._take(4, "i32"))[0])

    def read_u32(self) -> int:
        return int(_U32.unpack_from(self._data, self._take(4, "u32"))[0])

    def read_i64(self) -> int:
        return int(_I64.unpack_from(self._data, self._take(8, "i64"))[0])

    def read_u64(self) -> int:
        return int(_U64.unpack_from(self._data, self._take(8, "u64"))[0])

    def read_f32(self) -> float:
        return float(_F32.unpack_from(self._data, self._take(4, "f32"))[0])

    def read_f64(self) -> float:
        return float(_F64.unpack_from(self._data, self._take(8, "f64"))[0])

    def read_count(self) -> int:
        # An element count, not a byte length: elements can encode to zero
        # bytes, so it is not bounded by the remaining data.
        return int(_U32.unpack_from(self._data, self._take(4, "count"))[0])

    def read_flag(self) -> bool:
        b = self._data[self._take(1, "option flag")]
        if b > 1:
            raise _malformed("invalid option flag")
        return b == 1

    def read_bytes(self) -> bytes:
        n = int(_U32.unpack_from(self._data, self._take(4, "length"))[0])
        pos = self._take(n, "bytes")
        return self._data[pos:pos + n]

    def read_string(self) -> str:
        try:
            return self.read_bytes().decode("utf-8")
        except UnicodeDecodeError:
            raise _malformed("string is not valid UTF-8") from None

    def read_numbers(self, code: str, size: int) -> List[Any]:
        """A list of one fixed-width numeric primitive, unpacked in one call."""
        n = self.read_count()
        pos = self._take(n * size, "list")
        return list(struct.unpack_from(f"<{n}{code}", self._data, pos))

    def read_object(self) -> int:
        """An object token: one strong reference the caller adopts."""
        ptr = int(_U64.unpack_from(self._data, self._take(8, "object token"))[0])
        if ptr == 0:
            raise _malformed("null object token")
        return ptr

    def expect_end(self) -> None:
        if self._pos != len(self._data):
            raise _malformed("trailing bytes")


def _decode(data: bytes, read: Callable[[_Reader], Any]) -> Any:
    """Decode exactly one value from `data`."""
    r = _Reader(data)
    value = read(r)
    r.expect_end()
    return value
