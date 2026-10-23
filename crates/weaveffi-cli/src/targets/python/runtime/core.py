"""ctypes bindings for the {{NAME}} native library."""
from __future__ import annotations

{{IMPORTS}}import array
import builtins
import ctypes
import operator
import os
import platform
import struct
import threading
import warnings
from collections.abc import Callable, Iterable, Iterator, Mapping, Sequence
from dataclasses import dataclass
from enum import IntEnum
from typing import Any, ClassVar, Generic, TypeVar


class {{ERROR}}(Exception):
    """An error a fallible call reports.

    `code` is positive for one of the API's declared error codes, raised as
    the domain's typed subclasses, and negative for a runtime failure of a
    call that throws: a generic failure (-1, also every failure of a call
    declared `throws: any`), a producer panic (-2), a marshalling failure
    (-3), or a failed callback-interface implementation (-4). A cancelled
    async call (-5) raises `asyncio.CancelledError` instead. A call that
    declares no errors never raises this; its failures are bugs and raise
    `{{TRAP}}`.
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

    def _payload(self) -> bytes:
        """The code's fields as a value buffer (a code with fields overrides
        this)."""
        return b""


class {{TRAP}}(RuntimeError):
    """A failure of a call that declares no errors: a producer panic (-2), a
    marshalling failure (-3), a callback-interface failure the producer let
    through (-4), or another runtime failure (-1). These are bugs, so they
    are unchecked; `code` and `message` say what went wrong.
    """

    def __init__(self, code: int, message: str) -> None:
        super().__init__(f"({code}) {message}")
        self.code = code
        self.message = message


class LibraryLoadError(ImportError):
    """The native library couldn't be loaded, or it isn't the library these
    bindings were generated for (another C ABI revision, or a declaration
    that's missing or changed). Raised while the package is imported, so
    catch it as `ImportError` around the import; `path` names the library.
    """


class _ErrorStruct(ctypes.Structure):
    """The C `{{PREFIX}}_error` out-parameter every call reports through."""

    _fields_ = [
        ("code", ctypes.c_int32),
        ("message_ptr", ctypes.c_void_p),
        ("message_len", ctypes.c_size_t),
        ("payload_ptr", ctypes.c_void_p),
        ("payload_len", ctypes.c_size_t),
    ]


def _open(path: str, what: str) -> ctypes.CDLL:
    try:
        return ctypes.CDLL(path)
    except OSError as exc:
        raise LibraryLoadError(
            f"cannot load {what}: {exc}", name=__name__, path=path
        ) from exc


def _load_library() -> ctypes.CDLL:
    # An explicit path wins; then a library bundled inside this package (a
    # platform wheel); then the system loader's search path.
    override = os.environ.get("{{LIBRARY_ENV}}")
    if override:
        return _open(override, f"the {{NAME}} native library from {{LIBRARY_ENV}}={override!r}")
    system = platform.system()
    if system == "Darwin":
        name = "{{LIB_DARWIN}}"
    elif system == "Windows":
        name = "{{LIB_WINDOWS}}"
    else:
        name = "{{LIB_LINUX}}"
    bundled = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    if os.path.exists(bundled):
        return _open(bundled, f"the bundled {{NAME}} native library {bundled!r}")
    return _open(
        name,
        f"the {{NAME}} native library {name!r} (set {{LIBRARY_ENV}} to its path, "
        "or install a wheel that bundles it)",
    )


_lib = _load_library()


def _bind(name: str, restype: Any, *argtypes: Any) -> Any:
    """Look up one exported symbol and fix its C signature. Every prototype
    is bound once, at import time."""
    fn = getattr(_lib, name)
    fn.restype = restype
    fn.argtypes = argtypes
    return fn


class _ContractEntry(ctypes.Structure):
    """The C `{{PREFIX}}_contract_entry`: one declaration's id and hash."""

    _fields_ = [("id", ctypes.c_uint64), ("hash", ctypes.c_uint64)]


# The C ABI revision these bindings were generated against and, per
# top-level module's contract symbol, the declarations they rely on:
# (id, hash, dotted path), with each declaration's canonical signature.
_ABI_VERSION = {{ABI_VERSION}}
_CONTRACTS: dict[str, list[tuple[int, int, str]]] = {
{{CONTRACTS}}
}


def _refuse(message: str) -> LibraryLoadError:
    return LibraryLoadError(f"{message} ({_lib._name})", name=__name__, path=_lib._name)


def _check_contract() -> None:
    """Refuse to load a library built for another ABI revision, or one that
    lacks or changed a declaration these bindings use, before any other
    symbol is bound."""
    try:
        abi_version = _bind("{{PREFIX}}_abi_version", ctypes.c_uint32)
    except AttributeError:
        raise _refuse(
            "the library doesn't export {{PREFIX}}_abi_version, so it isn't the "
            "{{NAME}} library these bindings were generated for"
        ) from None
    found = abi_version()
    if found != _ABI_VERSION:
        raise _refuse(
            f"C ABI mismatch: these bindings expect revision {_ABI_VERSION}, but the "
            f"library reports revision {found}; regenerate the bindings or rebuild "
            "the library"
        )
    for symbol, expected in _CONTRACTS.items():
        try:
            contract = _bind(
                symbol, ctypes.POINTER(_ContractEntry), ctypes.POINTER(ctypes.c_size_t)
            )
        except AttributeError:
            raise _refuse(f"the library doesn't export {symbol}") from None
        length = ctypes.c_size_t()
        table = contract(ctypes.byref(length))
        hashes = {table[i].id: table[i].hash for i in range(length.value)} if table else {}
        for entry_id, entry_hash, path in expected:
            actual = hashes.get(entry_id)
            if actual is None:
                raise _refuse(f"{path} is missing from the library")
            if actual != entry_hash:
                raise _refuse(f"{path} changed since these bindings were generated")


_check_contract()

_error_clear = _bind("{{PREFIX}}_error_clear", None, ctypes.POINTER(_ErrorStruct))
_free_bytes = _bind("{{PREFIX}}_free_bytes", None, ctypes.c_void_p, ctypes.c_size_t)
_debug_live_fn = _bind("{{PREFIX}}_debug_live", ctypes.c_uint64, ctypes.c_int32)


def _debug_live(kind: int) -> int:
    """The producer's live-resource counter `kind` (0 objects, 1 callbacks,
    2 iterators, 3 cancel tokens, 4 byte runs; -1 is 1 when the producer
    counts at all). Always 0 unless the producer was built with its leak
    counters enabled."""
    live: int = _debug_live_fn(kind)
    return live


def _peek_bytes(ptr: int | None, length: int) -> bytes:
    """Copy a byte run the producer only lends (a zero-length run's pointer
    is never read)."""
    return ctypes.string_at(ptr, length) if ptr and length else b""


def _read_error(err: _ErrorStruct) -> tuple[int, str, bytes]:
    """Copy a filled out-err slot's code, message, and payload, then release
    them."""
    code = err.code
    message = _peek_bytes(err.message_ptr, err.message_len).decode("utf-8", "replace")
    payload = _peek_bytes(err.payload_ptr, err.payload_len)
    _error_clear(ctypes.byref(err))
    return code, message, payload


def _trap_from(code: int, message: str, payload: bytes = b"") -> {{TRAP}}:
    """The exception for a failed call that declares no errors."""
    return {{TRAP}}(code, message)


def {{ERROR_FROM}}(code: int, message: str, payload: bytes = b"") -> {{ERROR}}:
    """The exception for a failed call declared `throws: any`."""
    return {{ERROR}}(code, message)


def _unknown_code(domain: type[{{ERROR}}], code: int, message: str) -> {{ERROR}}:
    """The exception for a code a domain's factory doesn't know: a positive
    code (from a library newer than these bindings) raises the domain's base
    class with the code and message; a runtime code raises the root error."""
    return domain(code, message) if code > 0 else {{ERROR}}(code, message)


def _take_bytes(ptr: int | None, length: int) -> bytes:
    """Copy a producer-owned byte run (a string, bytes, or value buffer) and
    release it with {{PREFIX}}_free_bytes."""
    if not ptr:
        return b""
    try:
        return ctypes.string_at(ptr, length)
    finally:
        _free_bytes(ptr, length)


def _take_str(ptr: int | None, length: int) -> str:
    """Copy and release a producer-owned UTF-8 string."""
    return _take_bytes(ptr, length).decode("utf-8")


# Range checks. ctypes truncates an out-of-range integer argument to the C
# width silently, so every integer crossing directly is checked first.


def _int_check(kind: str, lo: int, hi: int) -> Callable[[int, str], int]:
    def check(value: int, what: str) -> int:
        try:
            v = operator.index(value)
        except TypeError:
            raise TypeError(f"{what}: expected an integer, got {type(value).__name__}") from None
        if lo <= v <= hi:
            return v
        raise OverflowError(f"{what}: {v} is out of range for {kind}")

    return check


_i8 = _int_check("i8", -(1 << 7), (1 << 7) - 1)
_i16 = _int_check("i16", -(1 << 15), (1 << 15) - 1)
_i32 = _int_check("i32", -(1 << 31), (1 << 31) - 1)
_i64 = _int_check("i64", -(1 << 63), (1 << 63) - 1)
_u8 = _int_check("u8", 0, (1 << 8) - 1)
_u16 = _int_check("u16", 0, (1 << 16) - 1)
_u32 = _int_check("u32", 0, (1 << 32) - 1)
_u64 = _int_check("u64", 0, (1 << 64) - 1)


def _float(value: float, what: str) -> float:
    """A number as a float, rejecting anything else."""
    if isinstance(value, (int, float)):
        return float(value)
    raise TypeError(f"{what}: expected a number, got {type(value).__name__}")


# Typed arrays: numeric lists crossing directly. Each element kind's
# `array` typecode and ctypes element type.
_ARRAY_TYPES: dict[str, tuple[str, Any]] = {
    "i8": ("b", ctypes.c_int8),
    "i16": ("h", ctypes.c_int16),
    "i32": ("i", ctypes.c_int32),
    "i64": ("q", ctypes.c_int64),
    "u16": ("H", ctypes.c_uint16),
    "u32": ("I", ctypes.c_uint32),
    "u64": ("Q", ctypes.c_uint64),
    "f32": ("f", ctypes.c_float),
    "f64": ("d", ctypes.c_double),
}


def _array(values: Iterable[Any], kind: str, what: str) -> array.array[Any]:
    """Pack a sequence of numbers into a range-checked typed array whose
    buffer is lent to one call. An `array.array` of the matching typecode
    is lent as is, without a copy."""
    code = _ARRAY_TYPES[kind][0]
    if isinstance(values, array.array) and values.typecode == code:
        return values
    if isinstance(values, (str, bytes, bytearray, memoryview)):
        raise TypeError(f"{what}: expected a sequence of numbers, got {type(values).__name__}")
    try:
        return array.array(code, values)
    except OverflowError:
        raise OverflowError(f"{what}: an element is out of range for {kind}") from None
    except TypeError as exc:
        raise TypeError(f"{what}: {exc}") from None


def _peek_array(ptr: int | None, count: int, kind: str) -> list[Any]:
    """Copy a typed array the producer only lends."""
    if not ptr or not count:
        return []
    values: list[Any] = (_ARRAY_TYPES[kind][1] * count).from_address(ptr)[:]
    return values


def _take_array(ptr: int | None, count: int, kind: str) -> list[Any]:
    """Copy a producer-owned typed array and release it with
    {{PREFIX}}_free_bytes."""
    if not ptr:
        return []
    ctype = _ARRAY_TYPES[kind][1]
    try:
        values: list[Any] = (ctype * count).from_address(ptr)[:]
        return values
    finally:
        _free_bytes(ptr, count * ctypes.sizeof(ctype))


_H = TypeVar("_H", bound="_Handle")
_T = TypeVar("_T")


class _Handle:
    """Owns one native reference and releases it exactly once.

    Calls lend the pointer through `_acquire`/`_release`, so `close()` from
    another thread while a call is in flight defers the release until the
    last such call returns instead of freeing the object mid-call.
    """

    def _init_handle(self, ptr: int) -> None:
        self._ptr: int | None = ptr
        self._key = ptr
        self._calls = 0
        self._closing = False
        self._lock = threading.Lock()

    def _free(self, ptr: int) -> None:
        """Release the native reference (each subclass releases its kind)."""

    def _acquire(self) -> int:
        with self._lock:
            ptr = self._ptr
            if ptr is None or self._closing:
                raise ValueError(f"{type(self).__name__} is closed")
            self._calls += 1
            return ptr

    def _release(self) -> None:
        with self._lock:
            self._calls -= 1
            if self._calls or not self._closing:
                return
            ptr, self._ptr = self._ptr, None
        if ptr is not None:
            self._free(ptr)

    def close(self) -> None:
        """Release the native reference. Idempotent; the object is unusable
        afterwards."""
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
            self._free(ptr)

    def __enter__(self: _H) -> _H:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        # An unreachable handle has no call in flight: release directly.
        ptr = getattr(self, "_ptr", None)
        if ptr is not None:
            self._ptr = None
            self._free(ptr)


class _Object(_Handle):
    """Base of every interface wrapper: a reference-counted native object.
    Two wrappers are equal when they hold the same native object."""

    # The native destroy and clone functions, set per subclass.
    _destroy: ClassVar[Callable[[int], None]]
    _clone: ClassVar[Callable[[int], int]]

    @classmethod
    def _adopt(cls: type[_H], ptr: int) -> _H:
        """Wrap one strong reference the producer handed over."""
        obj = cls.__new__(cls)
        obj._init_handle(ptr)
        return obj

    def _free(self, ptr: int) -> None:
        type(self)._destroy(ptr)

    def _clone_ref(self) -> int:
        """A new strong reference to the same object, for a receiver that
        adopts it (an object token, or a callback method's return)."""
        ptr = self._acquire()
        try:
            return type(self)._clone(ptr)
        finally:
            self._release()

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, _Object):
            return NotImplemented
        return self is other or (self._ptr is not None and self._ptr == other._ptr)

    def __hash__(self) -> int:
        return hash(self._key)


class NativeIterator(Iterator[_T], _Handle, Generic[_T]):
    """A lazy iterator over a sequence the native library produces.

    Each step pulls one element from the producer. The native handle is
    released when the iterator is exhausted, on `close()`, at the end of a
    `with` block, or by garbage collection; a closed iterator is exhausted.
    Iterator-returning calls create these; there's no public constructor.
    """

    _pull: Callable[[int], _T]
    _destroy_fn: Callable[[int], None]

    def __init__(self) -> None:
        raise TypeError("NativeIterator objects come from the API's iterator-returning calls")

    def _free(self, ptr: int) -> None:
        self._destroy_fn(ptr)

    def __next__(self) -> _T:
        with self._lock:
            ptr = self._ptr
            if ptr is None or self._closing:
                raise StopIteration
            self._calls += 1
        try:
            item = self._pull(ptr)
        except StopIteration:
            self._release()
            self.close()
            raise
        except BaseException:
            self._release()
            raise
        self._release()
        return item

    def __repr__(self) -> str:
        state = "closed" if self._ptr is None or self._closing else "open"
        return f"<NativeIterator ({state})>"


def _iterate(
    ptr: int, pull: Callable[[int], _T], destroy: Callable[[int], None]
) -> NativeIterator[_T]:
    """Wrap an iterator handle the producer returned."""
    it: NativeIterator[_T] = NativeIterator.__new__(NativeIterator)
    it._init_handle(ptr)
    it._pull = pull
    it._destroy_fn = destroy
    return it


def _required(ptr: int | None) -> int:
    """A non-null object pointer the producer returned."""
    if not ptr:
        raise {{TRAP}}(-3, "the producer returned a null object")
    return ptr


def _lend(obj: Any, cls: type[_Object]) -> int:
    """Lend `obj`'s native pointer to one call; pair with `obj._release()`."""
    if not isinstance(obj, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(obj).__name__}")
    return obj._acquire()


def _lend_opt(obj: Any, cls: type[_Object]) -> int | None:
    """`_lend` for an optional object: None lends a null pointer."""
    return None if obj is None else _lend(obj, cls)


def _release_opt(obj: _Object | None) -> None:
    if obj is not None:
        obj._release()


_S_I8 = struct.Struct("<b")
_S_U8 = struct.Struct("<B")
_S_I16 = struct.Struct("<h")
_S_U16 = struct.Struct("<H")
_S_I32 = struct.Struct("<i")
_S_U32 = struct.Struct("<I")
_S_I64 = struct.Struct("<q")
_S_U64 = struct.Struct("<Q")
_S_F32 = struct.Struct("<f")
_S_F64 = struct.Struct("<d")

_E = TypeVar("_E", bound=IntEnum)


def _malformed(what: str) -> {{TRAP}}:
    return {{TRAP}}(-3, f"malformed value buffer: {what}")


def _bad_value(kind: str, value: object) -> Exception:
    """The exception for a value `struct` can't pack as `kind`: out of range
    for a number, else the wrong type."""
    if isinstance(value, (int, float)):
        return OverflowError(f"{value} is out of range for {kind}")
    return TypeError(f"expected a number for {kind}, got {type(value).__name__}")


class _Writer:
    """Encodes values in the value-buffer wire format: little-endian,
    packed, no alignment."""

    __slots__ = ("_buf", "_objects")

    def __init__(self) -> None:
        self._buf = bytearray()
        self._objects: list[tuple[int, _Object]] = []

    def finish(self) -> bytes:
        # Object tokens are minted last, so an encoding that fails part-way
        # never leaks a strong reference.
        minted: list[tuple[_Object, int]] = []
        try:
            for pos, obj in self._objects:
                ptr = obj._clone_ref()
                minted.append((obj, ptr))
                _S_U64.pack_into(self._buf, pos, ptr)
        except BaseException:
            for obj, ptr in minted:
                type(obj)._destroy(ptr)
            raise
        self._objects = []
        return bytes(self._buf)

    def _put(self, fmt: struct.Struct, kind: str, v: Any) -> None:
        try:
            self._buf += fmt.pack(v)
        except struct.error:
            raise _bad_value(kind, v) from None

    def write_bool(self, v: bool) -> None:
        self._buf.append(1 if v else 0)

    def write_i8(self, v: int) -> None:
        self._put(_S_I8, "i8", v)

    def write_u8(self, v: int) -> None:
        self._put(_S_U8, "u8", v)

    def write_i16(self, v: int) -> None:
        self._put(_S_I16, "i16", v)

    def write_u16(self, v: int) -> None:
        self._put(_S_U16, "u16", v)

    def write_i32(self, v: int) -> None:
        self._put(_S_I32, "i32", v)

    def write_u32(self, v: int) -> None:
        self._put(_S_U32, "u32", v)

    def write_i64(self, v: int) -> None:
        self._put(_S_I64, "i64", v)

    def write_u64(self, v: int) -> None:
        self._put(_S_U64, "u64", v)

    def write_f32(self, v: float) -> None:
        self._put(_S_F32, "f32", v)

    def write_f64(self, v: float) -> None:
        self._put(_S_F64, "f64", v)

    def write_count(self, n: int) -> None:
        self._buf += _S_U32.pack(n)

    def write_flag(self, present: bool) -> None:
        self._buf.append(1 if present else 0)

    def write_string(self, v: str) -> None:
        data = v.encode("utf-8")
        self._buf += _S_U32.pack(len(data))
        self._buf += data

    def write_bytes(self, v: bytes) -> None:
        self._buf += _S_U32.pack(len(v))
        self._buf += v

    def write_numbers(self, code: str, kind: str, values: Sequence[Any]) -> None:
        """A list of one fixed-width numeric primitive (`code` is its struct
        format character), packed in one call."""
        try:
            data = struct.pack(f"<{len(values)}{code}", *values)
        except struct.error:
            one = struct.Struct(f"<{code}")
            for v in values:
                try:
                    one.pack(v)
                except struct.error:
                    raise _bad_value(kind, v) from None
            raise
        self._buf += _S_U32.pack(len(values))
        self._buf += data

    def write_object(self, obj: Any, cls: type[_Object]) -> None:
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
    buffers, invalid flag bytes, invalid UTF-8, undeclared enum values, and
    trailing data."""

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
        return int(_S_I8.unpack_from(self._data, self._take(1, "i8"))[0])

    def read_u8(self) -> int:
        return self._data[self._take(1, "u8")]

    def read_i16(self) -> int:
        return int(_S_I16.unpack_from(self._data, self._take(2, "i16"))[0])

    def read_u16(self) -> int:
        return int(_S_U16.unpack_from(self._data, self._take(2, "u16"))[0])

    def read_i32(self) -> int:
        return int(_S_I32.unpack_from(self._data, self._take(4, "i32"))[0])

    def read_u32(self) -> int:
        return int(_S_U32.unpack_from(self._data, self._take(4, "u32"))[0])

    def read_i64(self) -> int:
        return int(_S_I64.unpack_from(self._data, self._take(8, "i64"))[0])

    def read_u64(self) -> int:
        return int(_S_U64.unpack_from(self._data, self._take(8, "u64"))[0])

    def read_f32(self) -> float:
        return float(_S_F32.unpack_from(self._data, self._take(4, "f32"))[0])

    def read_f64(self) -> float:
        return float(_S_F64.unpack_from(self._data, self._take(8, "f64"))[0])

    def read_count(self) -> int:
        # An element count, not a byte length: elements can encode to zero
        # bytes, so it isn't bounded by the remaining data.
        return int(_S_U32.unpack_from(self._data, self._take(4, "count"))[0])

    def read_flag(self) -> bool:
        b = self._data[self._take(1, "option flag")]
        if b > 1:
            raise _malformed("invalid option flag")
        return b == 1

    def read_bytes(self) -> bytes:
        n = int(_S_U32.unpack_from(self._data, self._take(4, "length"))[0])
        pos = self._take(n, "bytes")
        return self._data[pos:pos + n]

    def read_string(self) -> str:
        try:
            return self.read_bytes().decode("utf-8")
        except UnicodeDecodeError:
            raise _malformed("string is not valid UTF-8") from None

    def read_enum(self, cls: type[_E]) -> _E:
        value = self.read_i32()
        try:
            return cls(value)
        except ValueError:
            raise _malformed(f"undeclared {cls.__name__} value {value}") from None

    def read_numbers(self, code: str, size: int) -> list[Any]:
        """A list of one fixed-width numeric primitive, unpacked in one call."""
        n = self.read_count()
        pos = self._take(n * size, "list")
        return list(struct.unpack_from(f"<{n}{code}", self._data, pos))

    def read_object(self) -> int:
        """An object token: one strong reference the caller adopts."""
        ptr = int(_S_U64.unpack_from(self._data, self._take(8, "object token"))[0])
        if ptr == 0:
            raise _malformed("null object token")
        return ptr

    def expect_end(self) -> None:
        if self._pos != len(self._data):
            raise _malformed("trailing bytes")


def _decode(data: bytes, read: Callable[[_Reader], _T]) -> _T:
    """Decode exactly one value from `data`."""
    r = _Reader(data)
    value = read(r)
    r.expect_end()
    return value
