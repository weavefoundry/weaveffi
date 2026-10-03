# Callback-interface implementations the producer holds, keyed by the integer
# it receives as `ctx`. An entry is added when an implementation is passed to
# a call and removed when the producer calls the vtable's `free(ctx)`, so the
# implementation lives exactly as long as the producer may call it and the
# producer never holds a raw reference to a Python object. ctypes acquires
# the GIL on entry to a trampoline, so the producer may call from any thread.
_callbacks: Dict[int, Any] = {}
_callbacks_lock = threading.Lock()
_callbacks_next = 0

_error_set = _bind(
    "{{PREFIX}}_error_set", None, ctypes.POINTER(_ErrorStruct), ctypes.c_int32, ctypes.c_char_p
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


def _callback_get(ctx: Optional[int]) -> Any:
    return _callbacks[ctx or 0]


def _callback_free(ctx: Optional[int]) -> None:
    # The producer's last reference is gone; it never passes `ctx` again.
    with _callbacks_lock:
        _callbacks.pop(ctx or 0, None)


def _callback_fail(out_err: Any, exc: BaseException) -> None:
    """Report an implementation's exception to the producer, which aborts
    its call with the foreign error code. Nothing unwinds through C."""
    message = str(exc) or type(exc).__name__
    _error_set(out_err, {{ERROR}}.FOREIGN_ERROR_CODE, message.encode("utf-8", "replace"))
