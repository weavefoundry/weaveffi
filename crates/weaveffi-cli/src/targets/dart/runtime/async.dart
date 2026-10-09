// ── Async completion ──
// An async call completes through a `NativeCallable.listener` the producer
// invokes exactly once, from any thread. Everything the completion carries is
// owned by these bindings, so it's decoded later on the event loop and then
// released.

final _errorFree =
    _lib.lookupFunction<Void Function(Pointer<_Error>), void Function(Pointer<_Error>)>(
        '{{PREFIX}}_error_free');

/// Builds the error a completion reports and frees its boxed error.
Object _takeAsyncError(Pointer<_Error> err, _ErrorMapper map) {
  try {
    return _readError(err, map);
  } finally {
    _errorFree(err);
  }
}
