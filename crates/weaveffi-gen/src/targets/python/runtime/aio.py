# Async completions. A call registers its future under an integer key, passes
# the key as the launcher's `context`, and the function's static completion
# trampoline settles it from whichever producer thread completes the call.
# The trampolines live at module scope, so nothing a pending call needs can
# be garbage collected before the producer fires the completion, even when
# the awaiting coroutine is gone.
_pending: Dict[int, Tuple[asyncio.AbstractEventLoop, "asyncio.Future[Any]"]] = {}
_pending_lock = threading.Lock()
_pending_next = 0

_error_free = _bind("{{PREFIX}}_error_free", None, ctypes.POINTER(_ErrorStruct))


def _async_begin() -> Tuple[int, "asyncio.Future[Any]"]:
    """Register a new pending call on the running event loop."""
    global _pending_next
    loop = asyncio.get_running_loop()
    future = loop.create_future()
    with _pending_lock:
        _pending_next += 1
        key = _pending_next
        _pending[key] = (loop, future)
    return key, future


def _async_abandon(key: int) -> None:
    """Forget a call whose launcher never ran."""
    with _pending_lock:
        _pending.pop(key, None)


def _async_error(err: Any, factory: Callable[[int, str, bytes], BaseException]) -> BaseException:
    """Copy and release a heap-boxed completion error, then build the
    exception it reports. The cancelled code maps to CancelledError."""
    e = err.contents
    code = e.code
    message = e.message.decode("utf-8", "replace") if e.message else ""
    payload = ctypes.string_at(e.payload_ptr, e.payload_len) if e.payload_ptr else b""
    _error_free(err)
    if code == {{ERROR}}.CANCELLED_ERROR_CODE:
        return asyncio.CancelledError(message)
    return factory(code, message, payload)


def _async_settle(key: Optional[int], exc: Optional[BaseException], value: Any) -> None:
    """Hand a completion back to the event loop that awaits it."""
    with _pending_lock:
        entry = _pending.pop(key or 0, None)
    if entry is None:
        return
    loop, future = entry
    try:
        loop.call_soon_threadsafe(_async_resolve, future, exc, value)
    except RuntimeError:
        # The loop is closed, so nothing awaits the result any more.
        pass


def _async_resolve(future: "asyncio.Future[Any]", exc: Optional[BaseException], value: Any) -> None:
    if future.done():
        return
    if isinstance(exc, asyncio.CancelledError):
        future.cancel()
    elif exc is not None:
        future.set_exception(exc)
    else:
        future.set_result(value)
