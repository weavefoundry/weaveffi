# Cancellation for `cancellable` async functions. Each call creates a token,
# the producer takes its own reference at launch, and cancelling the awaiting
# task cancels the token; the producer then completes the call with the
# cancelled code.
_cancel_token_create = _bind("{{PREFIX}}_cancel_token_create", ctypes.c_void_p)
_cancel_token_cancel = _bind("{{PREFIX}}_cancel_token_cancel", None, ctypes.c_void_p)
_cancel_token_destroy = _bind("{{PREFIX}}_cancel_token_destroy", None, ctypes.c_void_p)


async def _async_wait_cancellable(future: asyncio.Future[Any], token: int) -> Any:
    """Await a cancellable call. Cancelling the awaiting task cancels the
    native call and waits for its (cancelled) completion before raising, so
    nothing outlives the call."""
    try:
        return await asyncio.shield(future)
    except asyncio.CancelledError:
        _cancel_token_cancel(token)
        try:
            await future
        except BaseException:
            pass
        raise
    finally:
        _cancel_token_destroy(token)
