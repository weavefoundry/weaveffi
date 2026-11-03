// ── Objects ──

/// The lifetime machinery every interface wrapper shares. A wrapper owns one
/// strong reference to its native object and releases it exactly once: in
/// [dispose], or through a GC finalizer when it's collected undisposed.
///
/// Every call borrows the pointer through [_enter] and [_leave]. A [dispose]
/// that arrives while a call is in flight (from a callback the call made)
/// only marks the wrapper; the reference is released when the last call
/// leaves, so the native object never disappears under a running call.
abstract base class _NativeObject implements Finalizable {
  _NativeObject(this._ptr) {
    _finalizer.attach(this, _ptr, detach: this);
  }

  final Pointer<Void> _ptr;
  int _calls = 0;
  bool _disposed = false;

  NativeFinalizer get _finalizer;
  void _destroy(Pointer<Void> ptr);
  Pointer<Void> _clone(Pointer<Void> ptr);

  /// Releases this wrapper's native reference. Safe to call more than once;
  /// the native object is dropped when its last reference (this one, another
  /// wrapper's, or the producer's) goes away. Using the wrapper afterwards
  /// throws a [StateError].
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    if (_calls == 0) _release();
  }

  void _release() {
    _finalizer.detach(this);
    _destroy(_ptr);
  }

  Pointer<Void> _enter() {
    if (_disposed) throw StateError('$runtimeType used after dispose()');
    _calls++;
    return _ptr;
  }

  void _leave() {
    if (--_calls == 0 && _disposed) _release();
  }

  /// A second strong reference, for an object token in a value buffer.
  Pointer<Void> _cloneRef() {
    final ptr = _enter();
    try {
      return _clone(ptr);
    } finally {
      _leave();
    }
  }
}

/// Borrows [object]'s pointer until [arena] is released.
Pointer<Void> _borrow(Arena arena, _NativeObject object) {
  final ptr = object._enter();
  arena.onReleaseAll(object._leave);
  return ptr;
}
