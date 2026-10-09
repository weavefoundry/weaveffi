// ── Errors and memory ──

/// The C `{{PREFIX}}_error` slot every fallible call writes. A non-zero
/// `code` carries a producer-owned `message` and, for a domain error with
/// fields, a value-buffer `payload`; `{{PREFIX}}_error_clear` releases both.
final class _Error extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
  external Pointer<Uint8> payloadPtr;
  @Size()
  external int payloadLen;
}

final _errorClear = _lib.lookupFunction<Void Function(Pointer<_Error>),
    void Function(Pointer<_Error>)>('{{PREFIX}}_error_clear', isLeaf: true);

final _freeBytes = _lib.lookupFunction<Void Function(Pointer<Uint8>, Size),
    void Function(Pointer<Uint8>, int)>('{{PREFIX}}_free_bytes', isLeaf: true);

// Synchronous calls on this isolate share one error slot and one length
// slot: each call reads (and clears) them right after it returns, before any
// other call can reuse them, including a nested call from a callback.
final Pointer<_Error> _err = calloc<_Error>();
final Pointer<Size> _outLen = calloc<Size>();

/// A failure the native library reported to a function declared to throw.
/// Domain errors extend this class with a positive [code]; a negative code is
/// one of the runtime constants below.
class NativeException implements Exception {
  /// Creates an exception carrying [code] and [message].
  NativeException(this.code, this.message);

  /// The producer reported an untyped error.
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

/// Maps a runtime code (or one the domain doesn't declare) onto its
/// exception, for a function declared to throw.
NativeException _runtimeException(int code, String message, Uint8List payload) =>
    code == NativeException.cancelledCode
        ? CancelledException(message)
        : NativeException(code, message);

/// Builds the error [err] reports, using [map].
Object _readError(Pointer<_Error> err, _ErrorMapper map) {
  final e = err.ref;
  final message = e.message == nullptr ? '' : e.message.toDartString();
  return map(e.code, message, _copyBytes(e.payloadPtr, e.payloadLen));
}

/// Throws the error a synchronous call left in [err], clearing the slot.
/// [map] is the domain's mapper for a function declared to throw.
void _check(Pointer<_Error> err, [_ErrorMapper map = _trap]) {
  if (err.ref.code == 0) return;
  final error = _readError(err, map);
  _errorClear(err);
  throw error;
}

/// Copies a borrowed native byte run into Dart memory.
Uint8List _copyBytes(Pointer<Uint8> ptr, int len) =>
    ptr == nullptr ? Uint8List(0) : Uint8List.fromList(ptr.asTypedList(len));

/// Copies an owned native byte run and releases it.
Uint8List _takeBytes(Pointer<Uint8> ptr, int len) {
  if (ptr == nullptr) return Uint8List(0);
  final bytes = Uint8List.fromList(ptr.asTypedList(len));
  _freeBytes(ptr, len);
  return bytes;
}

/// Decodes an owned UTF-8 run and releases it.
String _takeString(Pointer<Uint8> ptr, int len) {
  if (ptr == nullptr) return '';
  final text = utf8.decode(ptr.asTypedList(len));
  _freeBytes(ptr, len);
  return text;
}

/// Decodes a borrowed UTF-8 run.
String _readString(Pointer<Uint8> ptr, int len) =>
    ptr == nullptr ? '' : utf8.decode(ptr.asTypedList(len));

/// Copies [bytes] into [arena] memory for a borrowed `(ptr, len)` argument.
Pointer<Uint8> _stage(Arena arena, List<int> bytes) {
  if (bytes.isEmpty) return nullptr;
  final ptr = arena<Uint8>(bytes.length);
  ptr.asTypedList(bytes.length).setAll(0, bytes);
  return ptr;
}
