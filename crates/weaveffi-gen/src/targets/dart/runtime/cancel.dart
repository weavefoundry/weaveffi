// ── Cancellation ──

/// Cancels the cancellable calls it's passed to. One token can serve any
/// number of calls; once cancelled it stays cancelled, and a call that
/// receives a cancelled token completes with [CancelledException] without
/// waiting for its work.
final class CancelToken {
  bool _cancelled = false;
  final List<void Function()> _listeners = <void Function()>[];

  /// Whether [cancel] has been called.
  bool get isCancelled => _cancelled;

  /// Requests cancellation of every call this token was passed to. Calls
  /// that already completed are unaffected. Idempotent.
  void cancel() {
    if (_cancelled) return;
    _cancelled = true;
    final listeners = List.of(_listeners);
    _listeners.clear();
    for (final listener in listeners) {
      listener();
    }
  }
}

final _cancelTokenCreate =
    _lib.lookupFunction<Pointer<Void> Function(), Pointer<Void> Function()>(
        '{{PREFIX}}_cancel_token_create');
final _cancelTokenCancel = _lib.lookupFunction<Void Function(Pointer<Void>),
    void Function(Pointer<Void>)>('{{PREFIX}}_cancel_token_cancel');
final _cancelTokenDestroy = _lib.lookupFunction<Void Function(Pointer<Void>),
    void Function(Pointer<Void>)>('{{PREFIX}}_cancel_token_destroy');

/// The native token behind one launch: created for the call, cancelled when
/// the [CancelToken] is, and destroyed once the call completes. The producer
/// holds its own reference, so destroying ours never races the call.
final class _NativeCancel {
  _NativeCancel._(this._token) : pointer = _cancelTokenCreate() {
    if (_token.isCancelled) {
      _cancelTokenCancel(pointer);
    } else {
      _token._listeners.add(_cancel);
    }
  }

  /// Binds a native token to [token], or returns null for no token.
  static _NativeCancel? bind(CancelToken? token) =>
      token == null ? null : _NativeCancel._(token);

  final CancelToken _token;

  /// The `{{PREFIX}}_cancel_token*` passed to the launcher.
  final Pointer<Void> pointer;

  void _cancel() => _cancelTokenCancel(pointer);

  /// Detaches from the [CancelToken] and drops the native reference.
  void release() {
    _token._listeners.remove(_cancel);
    _cancelTokenDestroy(pointer);
  }
}
