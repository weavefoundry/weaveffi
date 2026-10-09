// ── Callback interfaces ──
// An implementation passed to the producer is parked in [_callbacks] under
// an integer key. The `ctx` the producer receives points at a small native
// [_CallbackContext] holding that key, so the producer never holds a Dart
// object and the GC never sees a raw pointer. The entry lives until the
// producer calls the vtable's `free`.
//
// Methods that return a value are `NativeCallable.isolateLocal`
// trampolines: they run synchronously, so the producer may only call them
// on this isolate's thread while a call from Dart is in progress. Every
// vtable is flagged thread-affine (`{{PREFIX}}_VTABLE_THREAD_AFFINE`), so
// the producer refuses such a call from any other thread, failing the
// method with -4 ("callback called off its thread") instead of making it
// (the VM would abort the process). Void methods and `free` are
// `NativeCallable.isolateGroupBound` forwarders, which may run on any
// thread: they copy their arguments into a message for this isolate
// (borrowed runs are copied before the forwarder returns) and the
// implementation runs later on the event loop, in the zone that passed the
// callback.

/// Vtable `flags` bit 0: value-returning methods may only be called on the
/// thread that passed the vtable.
const int _vtableThreadAffine = 1;

/// Where forwarders send messages: the allocator they build a message with
/// and the isolate port that receives it. Forwarders run outside the
/// isolate and can't read Dart globals, so everything they need is native.
final class _CallbackPost extends Struct {
  external Pointer<NativeFunction<Pointer<Void> Function(Size)>> alloc;
  external Pointer<NativeFunction<Void Function(Pointer<Void>)>> release;
  external Pointer<NativeFunction<Bool Function(Int64, Pointer<_CObject>)>> post;
  @Int64()
  external int port;
}

/// The native `ctx` of one registered implementation.
final class _CallbackContext extends Struct {
  @Int64()
  external int key;
  external Pointer<_CallbackPost> post;
}

// The `Dart_CObject` layout `Dart_PostCObject` reads.
final class _CArray extends Struct {
  @IntPtr()
  external int length;
  external Pointer<Pointer<_CObject>> values;
}

final class _CTypedData extends Struct {
  @Int32()
  external int type;
  @IntPtr()
  external int length;
  external Pointer<Uint8> values;
}

final class _CValue extends Union {
  @Bool()
  external bool asBool;
  @Int64()
  external int asInt64;
  @Double()
  external double asDouble;
  external _CArray asArray;
  external _CTypedData asTypedData;
}

final class _CObject extends Struct {
  @Int32()
  external int type;
  external _CValue value;
}

typedef _CallbackDispatch = void Function(
    Object impl, int method, List<Object?> message);

final class _CallbackEntry {
  _CallbackEntry(this.impl, this.dispatch, this.context) : zone = Zone.current;

  final Object impl;
  final _CallbackDispatch dispatch;
  final Pointer<_CallbackContext> context;
  final Zone zone;
}

final Map<int, _CallbackEntry> _callbacks = <int, _CallbackEntry>{};
int _nextCallbackKey = 1;

/// Registers [impl] and returns the `ctx` the producer receives.
Pointer<Void> _registerCallback(Object impl, _CallbackDispatch dispatch) {
  final key = _nextCallbackKey++;
  final context = calloc<_CallbackContext>();
  context.ref
    ..key = key
    ..post = _callbackPost;
  _callbacks[key] = _CallbackEntry(impl, dispatch, context);
  return context.cast();
}

/// The implementation registered under [ctx].
Object _callbackTarget(Pointer<Void> ctx) {
  final key = ctx.cast<_CallbackContext>().ref.key;
  final entry = _callbacks[key];
  if (entry == null) {
    throw StateError('callback context $key is not registered');
  }
  return entry.impl;
}

final Pointer<_CallbackPost> _callbackPost = () {
  final port = RawReceivePort(_receiveCallbackMessage, 'callback messages');
  port.keepIsolateAlive = false;
  _callbackPort = port;
  final allocator = Platform.isWindows
      ? DynamicLibrary.open('ole32.dll')
      : DynamicLibrary.process();
  final post = calloc<_CallbackPost>();
  post.ref
    ..alloc = allocator.lookup(Platform.isWindows ? 'CoTaskMemAlloc' : 'malloc')
    ..release = allocator.lookup(Platform.isWindows ? 'CoTaskMemFree' : 'free')
    ..post = NativeApi.postCObject.cast()
    ..port = port.sendPort.nativePort;
  return post;
}();

RawReceivePort? _callbackPort;

void _receiveCallbackMessage(Object? message) {
  final fields = message! as List<Object?>;
  final key = fields[0]! as int;
  final method = fields[1]! as int;
  if (method == _CallbackMessage.free) {
    final entry = _callbacks.remove(key);
    if (entry != null) calloc.free(entry.context);
    return;
  }
  final entry = _callbacks[key];
  if (entry == null) return;
  entry.zone.runGuarded(() => entry.dispatch(entry.impl, method, fields));
}

/// One message from a forwarder: `[key, method, arguments...]`, built in
/// native memory and posted with `Dart_PostCObject`, which copies it. Runs
/// outside the isolate, so it touches no Dart globals.
final class _CallbackMessage {
  _CallbackMessage(Pointer<Void> ctx, int method, int arguments)
      : _context = ctx.cast<_CallbackContext>().ref,
        _capacity = arguments + 2 {
    final post = _context.post.ref;
    final alloc = post.alloc.asFunction<Pointer<Void> Function(int)>();
    _objects = alloc(sizeOf<_CObject>() * (_capacity + 1)).cast();
    _slots = alloc(sizeOf<Pointer<_CObject>>() * _capacity).cast();
    int64(_context.key);
    int64(method);
  }

  static const int free = -1;
  static const int _kNull = 0;
  static const int _kBool = 1;
  static const int _kInt64 = 3;
  static const int _kDouble = 4;
  static const int _kArray = 6;
  static const int _kTypedData = 7;

  // `Dart_TypedData_Type` values: a posted array arrives as the matching
  // typed list (`Int32List`, `Float64List`, ...).
  static const int typedInt8 = 1;
  static const int typedUint8 = 2;
  static const int typedInt16 = 4;
  static const int typedUint16 = 5;
  static const int typedInt32 = 6;
  static const int typedUint32 = 7;
  static const int typedInt64 = 8;
  static const int typedUint64 = 9;
  static const int typedFloat32 = 10;
  static const int typedFloat64 = 11;

  final _CallbackContext _context;
  final int _capacity;
  late final Pointer<_CObject> _objects;
  late final Pointer<Pointer<_CObject>> _slots;
  int _count = 0;

  _CObject _next(int type) {
    final object = _objects + (_count + 1);
    _slots[_count++] = object;
    return object.ref..type = type;
  }

  void boolean(bool v) => _next(_kBool).value.asBool = v;

  void int64(int v) => _next(_kInt64).value.asInt64 = v;

  void float64(double v) => _next(_kDouble).value.asDouble = v;

  void pointer(Pointer<Void> v) => int64(v.address);

  /// An absent optional: arrives as `null`.
  void none() => _next(_kNull);

  void maybeBool(bool present, bool v) => present ? boolean(v) : none();

  void maybeInt64(bool present, int v) => present ? int64(v) : none();

  void maybeFloat64(bool present, double v) => present ? float64(v) : none();

  /// A borrowed byte run; posting copies it.
  void bytes(Pointer<Uint8> ptr, int len) => typedData(ptr, len, typedUint8);

  /// A borrowed array of [count] elements of the typed-data [type]; posting
  /// copies it. A run of length 0 is never read.
  void typedData(Pointer<NativeType> ptr, int count, int type) {
    final data = _next(_kTypedData).value.asTypedData;
    data
      ..type = type
      ..length = count
      ..values = count == 0 ? nullptr : ptr.cast();
  }

  void send() {
    final post = _context.post.ref;
    _objects.ref.type = _kArray;
    _objects.ref.value.asArray
      ..length = _count
      ..values = _slots;
    post.post.asFunction<bool Function(int, Pointer<_CObject>)>()(
        post.port, _objects);
    final release = post.release.asFunction<void Function(Pointer<Void>)>();
    release(_slots.cast());
    release(_objects.cast());
  }
}

// The vtable `free` entry every callback interface shares.
void _forwardCallbackFree(Pointer<Void> ctx) =>
    _CallbackMessage(ctx, _CallbackMessage.free, 0).send();

final Pointer<NativeFunction<Void Function(Pointer<Void>)>> _callbackFree =
    _pin(NativeCallable<Void Function(Pointer<Void>)>.isolateGroupBound(
        _forwardCallbackFree));

// Vtable entries live for the process: they're never closed, so they're
// anchored here, and they don't keep an idle isolate alive.
final List<NativeCallable<Function>> _pinned = <NativeCallable<Function>>[];

Pointer<NativeFunction<T>> _pin<T extends Function>(NativeCallable<T> callable) {
  callable.keepIsolateAlive = false;
  _pinned.add(callable);
  return callable.nativeFunction;
}

final _errorSet = _lib.lookupFunction<
    Void Function(Pointer<_Error>, Int32, Pointer<Uint8>, Size),
    void Function(
        Pointer<_Error>, int, Pointer<Uint8>, int)>('{{PREFIX}}_error_set');

final _errorSetPayload = _lib.lookupFunction<
    Void Function(Pointer<_Error>, Pointer<Uint8>, Size),
    void Function(
        Pointer<_Error>, Pointer<Uint8>, int)>('{{PREFIX}}_error_set_payload');

/// Reports a failed callback method to the producer through [outErr]:
/// `{{PREFIX}}_error_set` copies [message], and `{{PREFIX}}_error_set_payload`
/// copies a domain code's encoded fields. Runs in the catch path of a
/// trampoline, which an exception must never unwind through.
void _failCallback(
    Pointer<_Error> outErr, int code, String message, Uint8List? payload) {
  if (outErr == nullptr) return;
  final text = utf8.encode(message);
  final scratch = calloc<Uint8>(text.length + (payload?.length ?? 0) + 1);
  try {
    scratch.asTypedList(text.length).setAll(0, text);
    _errorSet(outErr, code, scratch, text.length);
    if (payload == null || payload.isEmpty) return;
    scratch.asTypedList(payload.length).setAll(0, payload);
    _errorSetPayload(outErr, scratch, payload.length);
  } finally {
    calloc.free(scratch);
  }
}

/// Reports [error], an exception that isn't a domain error the method
/// declares, with [code] and the exception's text (a [NativeException]'s
/// message).
void _reportCallbackError(Pointer<_Error> outErr, int code, Object error) {
  try {
    final message = error is NativeException ? error.message : '$error';
    _failCallback(outErr, code, message, null);
  } catch (_) {
    _errorSet(outErr, code, nullptr, 0);
  }
}

/// Hands [bytes] to the producer through a method's `out_ptr`/`out_len`
/// slots as a run allocated with `{{PREFIX}}_alloc`, which the producer
/// adopts and frees. An empty run is null with length 0.
void _handOver(
    List<int> bytes, Pointer<Pointer<Uint8>> outPtr, Pointer<Size> outLen) {
  final run = bytes.isEmpty ? nullptr : _alloc(bytes.length);
  if (run != nullptr) run.asTypedList(bytes.length).setAll(0, bytes);
  outPtr.value = run;
  outLen.value = run == nullptr ? 0 : bytes.length;
}
