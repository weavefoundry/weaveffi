# Callback-interface implementations the producer holds, keyed by the integer
# it receives as `ctx`. An entry is added when an implementation is passed to
# a call and removed when the producer calls the vtable's `free(ctx)`, so the
# implementation lives exactly as long as the producer may call it and the
# producer never holds a raw reference to a Python object. ctypes acquires
# the GIL on entry to a trampoline, so the producer may call (and free) from
# any thread.
_callbacks: Dict[int, Any] = {}
_callbacks_lock = threading.Lock()
_callbacks_next = 0

_alloc = _bind("{{PREFIX}}_alloc", ctypes.c_void_p, ctypes.c_size_t)
_error_set = _bind(
    "{{PREFIX}}_error_set", None, ctypes.POINTER(_ErrorStruct), ctypes.c_int32, ctypes.c_char_p
)
_error_set_payload = _bind(
    "{{PREFIX}}_error_set_payload",
    None,
    ctypes.POINTER(_ErrorStruct),
    ctypes.c_char_p,
    ctypes.c_size_t,
)


def _callback_register(impl: Any, cls: type) -> int:
    """Register an implementation for one call; returns its `ctx` key."""
    global _callbacks_next
    if not isinstance(impl, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(impl).__name__}")
    with _callbacks_lock:
        _callbacks_next += 1
        ctx = _callbacks_next
        _callbacks[ctx] = impl
    return ctx


def _callback_register_opt(impl: Any, cls: type) -> Optional[int]:
    """`_callback_register` for an optional callback: None passes none."""
    return None if impl is None else _callback_register(impl, cls)


def _callback_get(ctx: Optional[int]) -> Any:
    return _callbacks[ctx or 0]


def _callback_free(ctx: Optional[int]) -> None:
    # The producer's last reference is gone; it never passes `ctx` again.
    with _callbacks_lock:
        _callbacks.pop(ctx or 0, None)


# Every vtable's `free` entry: one function object for the process lifetime.
_CallbackFree = ctypes.CFUNCTYPE(None, ctypes.c_void_p)
_callback_free_fn = _CallbackFree(_callback_free)


def _callback_return_bytes(out_ptr: Any, out_len: Any, data: bytes) -> None:
    """Hand a string, bytes, or value-buffer return to the producer: a run
    allocated with {{PREFIX}}_alloc, which the producer adopts and frees."""
    n = len(data)
    ptr = _alloc(n) if n else None
    if n:
        if not ptr:
            raise MemoryError(f"{{PREFIX}}_alloc({n}) failed")
        ctypes.memmove(ptr, data, n)
    out_ptr[0] = ptr
    out_len[0] = n


def _callback_return_object(obj: Any, cls: Type[_Object]) -> int:
    """Hand an object return to the producer: one fresh strong reference it
    adopts."""
    if not isinstance(obj, cls):
        raise TypeError(f"expected {cls.__name__}, got {type(obj).__name__}")
    return obj._clone_ref()


def _callback_return_object_opt(obj: Any, cls: Type[_Object]) -> Optional[int]:
    """`_callback_return_object` for an optional object: None returns null."""
    return None if obj is None else _callback_return_object(obj, cls)


def _callback_fail(out_err: Any, exc: BaseException, domain: Optional[Type[{{ERROR}}]] = None) -> None:
    """Report an implementation's exception to the producer. A method that
    declares errors passes its module's error domain, and an exception of
    that domain carrying a declared code travels as that code with its
    fields; every other exception reaches the producer as the foreign error
    code (-4). Nothing unwinds through C."""
    if domain is not None and isinstance(exc, domain) and exc.code > 0:
        _error_set(out_err, exc.code, exc.message.encode("utf-8", "replace"))
        payload = exc._payload()
        if payload:
            _error_set_payload(out_err, payload, len(payload))
        return
    if isinstance(exc, ({{ERROR}}, {{TRAP}})):
        message = exc.message
    else:
        message = str(exc) or type(exc).__name__
    _error_set(out_err, {{ERROR}}.FOREIGN_ERROR_CODE, message.encode("utf-8", "replace"))
