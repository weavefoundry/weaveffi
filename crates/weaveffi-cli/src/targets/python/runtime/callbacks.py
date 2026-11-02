# Callback-interface implementations the producer holds, keyed by the integer
# it receives as `ctx`. An entry is added when an implementation is passed to
# a call and removed when the producer calls the vtable's `free(ctx)`, so the
# implementation lives exactly as long as the producer may call it and the
# producer never holds a raw reference to a Python object. ctypes acquires
# the GIL on entry to a trampoline, so the producer may call (and free) from
# any thread.
_callbacks: dict[int, Any] = {}
_callbacks_lock = threading.Lock()
_callbacks_next = 0

_alloc = _bind("{{PREFIX}}_alloc", ctypes.c_void_p, ctypes.c_size_t)
_error_set = _bind(
    "{{PREFIX}}_error_set",
    None,
    ctypes.c_void_p,
    ctypes.c_int32,
    ctypes.c_char_p,
    ctypes.c_size_t,
)
_error_set_payload = _bind(
    "{{PREFIX}}_error_set_payload",
    None,
    ctypes.c_void_p,
    ctypes.c_char_p,
    ctypes.c_size_t,
)


def _callback_vtable(impl: Any, cls: type, vtable: Callable[[], int]) -> int:
    """Check an implementation passed to one call, and return its
    interface's vtable."""
    if not isinstance(impl, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(impl).__name__}")
    return vtable()


def _callback_vtable_opt(impl: Any, cls: type, vtable: Callable[[], int]) -> int | None:
    """`_callback_vtable` for an optional callback: None passes a null
    vtable."""
    return None if impl is None else _callback_vtable(impl, cls, vtable)


def _callback_register(impl: Any) -> int:
    """Register a checked implementation for one call; returns its `ctx`
    key."""
    global _callbacks_next
    with _callbacks_lock:
        _callbacks_next += 1
        ctx = _callbacks_next
        _callbacks[ctx] = impl
    return ctx


def _callback_register_opt(impl: Any) -> int | None:
    """`_callback_register` for an optional callback: None registers
    nothing."""
    return None if impl is None else _callback_register(impl)


def _callback_get(ctx: int | None) -> Any:
    return _callbacks[ctx or 0]


def _callback_free(ctx: int | None) -> None:
    # The producer's last reference is gone; it never passes `ctx` again.
    with _callbacks_lock:
        _callbacks.pop(ctx or 0, None)


# Every vtable entry's function object, kept for the process lifetime.
_vtable_keep: list[Any] = []


def _vtable_entry(fn: Callable[..., Any], restype: Any, *argtypes: Any) -> int:
    """The address of a function the producer calls as one vtable entry
    (a method returning nothing, a scalar, or an object)."""
    entry = ctypes.CFUNCTYPE(restype, *argtypes)(fn)
    _vtable_keep.append(entry)
    return ctypes.cast(entry, ctypes.c_void_p).value or 0


# Every vtable's `free` entry.
_callback_free_ptr = _vtable_entry(_callback_free, None, ctypes.c_void_p)


class _LazyVtable:
    """A callback interface's one vtable, built the first time an
    implementation is passed and kept for the process lifetime."""

    def __init__(self, build: Callable[[], ctypes.Structure]) -> None:
        self._build = build
        self._vtable: ctypes.Structure | None = None
        self._lock = threading.Lock()

    def __call__(self) -> int:
        with self._lock:
            if self._vtable is None:
                self._vtable = self._build()
            return ctypes.addressof(self._vtable)


def _alloc_run(data: bytes) -> _Run:
    """Hand a string, bytes, or value-buffer return to the producer: a run
    allocated with {{PREFIX}}_alloc, which the producer adopts and frees."""
    n = len(data)
    if not n:
        return _Run()
    ptr = _alloc(n)
    if not ptr:
        raise MemoryError(f"{{PREFIX}}_alloc({n}) failed")
    ctypes.memmove(ptr, data, n)
    return _Run(ptr, n)


def _alloc_array(values: Iterable[Any], kind: str, what: str) -> _Run:
    """Hand a typed-array return to the producer: a range-checked run
    allocated with {{PREFIX}}_alloc, which the producer adopts and frees."""
    data = _array(values, kind, what)
    size = len(data) * data.itemsize
    if not size:
        return _Run()
    ptr = _alloc(size)
    if not ptr:
        raise MemoryError(f"{{PREFIX}}_alloc({size}) failed")
    ctypes.memmove(ptr, data.buffer_info()[0], size)
    return _Run(ptr, size)


def _callback_return_object(obj: Any, cls: type[_Object]) -> int:
    """Hand an object return to the producer: one fresh strong reference it
    adopts."""
    if not isinstance(obj, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(obj).__name__}")
    return obj._clone_ref()


def _callback_return_object_opt(obj: Any, cls: type[_Object]) -> int | None:
    """`_callback_return_object` for an optional object: None returns null."""
    return None if obj is None else _callback_return_object(obj, cls)


# The codes a failed callback method reports when it raised anything but a
# code of its domain: -1 for a method that throws (a domain or `any`), -4
# for one that declares no errors.
_GENERIC = {{ERROR}}.GENERIC_ERROR_CODE
_FOREIGN = {{ERROR}}.FOREIGN_ERROR_CODE


def _callback_fail(
    out_err: int | None, exc: BaseException, code: int, domain: type[{{ERROR}}] | None = None
) -> None:
    """Report an implementation's exception to the producer. A method that
    throws an error domain passes it, and an exception of that domain
    carrying a positive code travels as that code with its fields as the
    payload; every other exception travels as `code` with its message.
    Nothing unwinds through C."""
    if domain is not None and isinstance(exc, domain) and exc.code > 0:
        code = exc.code
        payload = exc._payload()
        message = exc.message
    else:
        payload = b""
        if isinstance(exc, ({{ERROR}}, {{TRAP}})):
            message = exc.message
        else:
            message = str(exc) or type(exc).__name__
    data = message.encode("utf-8", "replace")
    _error_set(out_err, code, data, len(data))
    if payload:
        _error_set_payload(out_err, payload, len(payload))
