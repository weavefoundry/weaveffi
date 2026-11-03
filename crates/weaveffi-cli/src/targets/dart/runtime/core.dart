// ── Errors, call frames, and runs ──

/// The C `{{PREFIX}}_error` slot every fallible call writes. A non-zero
/// `code` carries a producer-owned UTF-8 message run (not NUL-terminated;
/// null with length 0 is the empty message) and, for a domain error with
/// fields, a value-buffer payload; `{{PREFIX}}_error_clear` releases both.
final class _Error extends Struct {
  @Int32()
  external int code;
  external Pointer<Uint8> messagePtr;
  @Size()
  external int messageLen;
  external Pointer<Uint8> payloadPtr;
  @Size()
  external int payloadLen;
}

/// The C `{{PREFIX}}_run`: a string's UTF-8, raw bytes, a typed array, or a
/// value buffer, crossing by value in every position. `len` counts bytes,
/// and null with length 0 is the empty run.
final class _Run extends Struct {
  external Pointer<Uint8> ptr;
  @Size()
  external int len;
}

final _errorClear = _lib.lookupFunction<Void Function(Pointer<_Error>),
    void Function(Pointer<_Error>)>('{{PREFIX}}_error_clear', isLeaf: true);

/// `{{PREFIX}}_run_free`: releases a run the producer returned.
final _runFree = _lib.lookupFunction<Void Function(_Run), void Function(_Run)>(
    '{{PREFIX}}_run_free',
    isLeaf: true);

/// `{{PREFIX}}_alloc`: an 8-aligned, zero-filled run the producer adopts
/// (and frees), for what a callback hands back.
final _alloc = _lib.lookupFunction<Pointer<Uint8> Function(Size),
    Pointer<Uint8> Function(int)>('{{PREFIX}}_alloc', isLeaf: true);

/// The native scratch of one call: its `{{PREFIX}}_error` slot, a 16-byte
/// out slot (an iterator's `out_item`), and an [Arena] for the arguments it
/// stages.
///
/// Every call takes its own frame and gives it back when it returns, so a
/// call made from a callback while another call is in progress never
/// touches the outer call's slots. Frames are pooled per isolate, so a call
/// allocates nothing in the steady state; a pooled frame's memory is freed
/// when the isolate goes away.
final class _Frame implements Finalizable {
  _Frame._() : _base = calloc<Uint8>(_size) {
    _finalizer.attach(this, _base.cast());
  }

  static final _finalizer = NativeFinalizer(calloc.nativeFree);

  /// The error slot, rounded up so the out slot is 8-aligned.
  static final int _errorSize = (sizeOf<_Error>() + 7) & ~7;
  static final int _size = _errorSize + 16;
  static final List<_Frame> _pool = <_Frame>[];

  final Pointer<Uint8> _base;
  Arena? _arena;

  /// A frame for one call: a pooled one, or a new one.
  static _Frame take() => _pool.isEmpty ? _Frame._() : _pool.removeLast();

  /// The call's `out_err` slot.
  Pointer<_Error> get err => _base.cast();

  /// The out slot; `cast` it to the slot's C type.
  Pointer<Uint8> get out => _base + _errorSize;

  /// The arena staged arguments live in until the call returns.
  Arena get arena => _arena ??= Arena();

  /// Throws the error the call left in [err], clearing the slot; [map] is
  /// the callable's error mapper.
  void check([_ErrorMapper map = _trap]) => _check(err, map);

  /// Releases what the call staged and returns the frame to the pool.
  void release() {
    _arena?.releaseAll(reuse: true);
    _pool.add(this);
  }
}

/// A failure the native library reported to a function declared to throw.
/// Domain errors extend this class with a positive [code]; a negative code is
/// one of the runtime constants below.
class NativeException implements Exception {
  /// Creates an exception carrying [code] and [message].
  NativeException(this.code, this.message);

  /// The producer reported an untyped error (a `throws any` function).
  static const int genericCode = -1;

  /// The producer panicked; [message] carries the panic text.
  static const int panicCode = -2;

  /// An argument or a callback's return couldn't be lifted by the producer.
  static const int marshalCode = -3;

  /// A callback implementation failed; [message] carries its text.
  static const int foreignCode = -4;

  /// The call was cancelled (see [CancelledException]).
  static const int cancelledCode = -5;

  /// The error code: positive for a domain error, negative for a runtime
  /// failure.
  final int code;

  /// The producer's message.
  final String message;

  @override
  String toString() => '$runtimeType($code): $message';
}

/// A cancellable call completed because its token was cancelled.
class CancelledException extends NativeException {
  /// Creates a cancellation with the producer's [message].
  CancelledException([String message = 'cancelled'])
      : super(NativeException.cancelledCode, message);
}

/// A function that isn't declared to throw failed anyway. That's a bug in
/// the native library (a panic, an argument it couldn't lift, a callback
/// failure it let through), so it's an [Error], not an [Exception].
final class NativeError extends Error {
  /// Creates the error a failed call reported.
  NativeError(this.code, this.message);

  /// The runtime code the producer reported (see [NativeException]).
  final int code;

  /// The producer's message.
  final String message;

  @override
  String toString() => 'NativeError($code): $message';
}

/// Maps a reported code, message, and payload onto the object to throw.
typedef _ErrorMapper = Object Function(
    int code, String message, Uint8List payload);

/// The mapper of a function that can't fail: every code but cancellation
/// is a [NativeError].
Object _trap(int code, String message, Uint8List payload) =>
    code == NativeException.cancelledCode
        ? CancelledException(message)
        : NativeError(code, message);

/// The mapper of a `throws any` function, and the fallback of a domain's
/// mapper for runtime codes: a [NativeException] (a [CancelledException]
/// for cancellation).
NativeException _runtimeException(
        int code, String message, Uint8List payload) =>
    code == NativeException.cancelledCode
        ? CancelledException(message)
        : NativeException(code, message);

/// Builds the error [err] reports, using [map].
Object _readError(Pointer<_Error> err, _ErrorMapper map) {
  final e = err.ref;
  final message = _readUtf8(e.messagePtr, e.messageLen);
  final payload = e.payloadLen == 0
      ? Uint8List(0)
      : Uint8List.fromList(e.payloadPtr.asTypedList(e.payloadLen));
  return map(e.code, message, payload);
}

/// Throws the error a synchronous call left in [err], clearing the slot
/// whether or not the error decodes. [map] is the callable's error mapper.
void _check(Pointer<_Error> err, [_ErrorMapper map = _trap]) {
  if (err.ref.code == 0) return;
  final Object error;
  try {
    error = _readError(err, map);
  } finally {
    _errorClear(err);
  }
  throw error;
}

String _readUtf8(Pointer<Uint8> ptr, int len) => len == 0
    ? ''
    : utf8.decode(ptr.asTypedList(len), allowMalformed: true);

/// A run of [len] bytes at [ptr].
_Run _run(Pointer<Uint8> ptr, int len) => Struct.create<_Run>()
  ..ptr = ptr
  ..len = len;

/// The empty run: what a failed callback returns.
_Run _emptyRun() => _run(nullptr, 0);

/// A view of a run's bytes, valid while the run is. The empty run is never
/// read (its pointer may be null).
Uint8List _viewRun(_Run run) =>
    run.len == 0 ? Uint8List(0) : run.ptr.asTypedList(run.len);

/// Copies a borrowed run's bytes into Dart memory.
Uint8List _copyRun(_Run run) => Uint8List.fromList(_viewRun(run));

/// Copies an owned run's bytes and releases the run.
Uint8List _takeRun(_Run run) {
  try {
    return _copyRun(run);
  } finally {
    _runFree(run);
  }
}

/// Decodes a borrowed UTF-8 run.
String _readText(_Run run) => _readUtf8(run.ptr, run.len);

/// Decodes an owned UTF-8 run and releases it.
String _takeText(_Run run) {
  try {
    return _readText(run);
  } finally {
    _runFree(run);
  }
}

/// Copies [bytes] into [arena] memory, borrowed by the producer for one
/// call. Arena memory comes from `calloc`, so it's aligned for any element.
_Run _stage(Arena arena, List<int> bytes) {
  if (bytes.isEmpty) return _emptyRun();
  final ptr = arena<Uint8>(bytes.length);
  ptr.asTypedList(bytes.length).setAll(0, bytes);
  return _run(ptr, bytes.length);
}

/// Stages [text]'s UTF-8 for one call.
_Run _stageText(Arena arena, String text) => _stage(arena, utf8.encode(text));

/// Hands [bytes] to the producer as a run allocated with `{{PREFIX}}_alloc`,
/// which the producer adopts and frees: what a callback method returns.
_Run _handOver(List<int> bytes) {
  if (bytes.isEmpty) return _emptyRun();
  final ptr = _alloc(bytes.length);
  ptr.asTypedList(bytes.length).setAll(0, bytes);
  return _run(ptr, bytes.length);
}
