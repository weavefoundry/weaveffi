// ── Typed arrays ──
// A numeric list (`[i32]`, `[f64]`, ...) crosses a call boundary as a C
// array and its element count, not as a value buffer. An argument is copied
// into arena memory for the call; a returned array is copied into a typed
// list (an `Int32List`, a `Float64List`, ...) and released with
// `{{PREFIX}}_free_bytes`; a callback's array argument is copied before the
// method runs; and a callback's returned array is handed over as a
// `{{PREFIX}}_alloc` run the producer adopts. A `u64` element travels as
// its two's-complement bit pattern, like every other `u64`.

/// Copies [data]'s bytes into [arena] memory (8-aligned), or returns null
/// when it's empty.
Pointer<Uint8> _stageData(Arena arena, TypedData data) {
  final n = data.lengthInBytes;
  if (n == 0) return nullptr;
  final ptr = arena<Uint8>(n);
  ptr.asTypedList(n).setAll(0, Uint8List.sublistView(data));
  return ptr;
}

/// Hands [count] elements of [data] to the producer through a callback's
/// `out_ptr`/`out_len` slots as a `{{PREFIX}}_alloc` run, which the producer
/// adopts. An empty array is null with count 0.
void _handOverData(TypedData data, int count, Pointer<Pointer<Uint8>> outPtr,
    Pointer<Size> outLen) {
  final n = data.lengthInBytes;
  final run = n == 0 ? nullptr : _alloc(n);
  if (run != nullptr) run.asTypedList(n).setAll(0, Uint8List.sublistView(data));
  outPtr.value = run;
  outLen.value = run == nullptr ? 0 : count;
}

/// Releases an owned array of [count] elements of [size] bytes.
void _freeArray(Pointer<NativeType> ptr, int count, int size) {
  if (count != 0) _freeBytes(ptr.cast(), count * size);
}

Pointer<Int8> _stageI8s(Arena a, List<int> v) =>
    _stageData(a, v is Int8List ? v : Int8List.fromList(v)).cast();
List<int> _copyI8s(Pointer<Int8> p, int n) =>
    n == 0 ? Int8List(0) : Int8List.fromList(p.asTypedList(n));
List<int> _takeI8s(Pointer<Int8> p, int n) {
  final v = _copyI8s(p, n);
  _freeArray(p, n, 1);
  return v;
}

void _handOverI8s(
        List<int> v, Pointer<Pointer<Int8>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Int8List ? v : Int8List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Int16> _stageI16s(Arena a, List<int> v) =>
    _stageData(a, v is Int16List ? v : Int16List.fromList(v)).cast();
List<int> _copyI16s(Pointer<Int16> p, int n) =>
    n == 0 ? Int16List(0) : Int16List.fromList(p.asTypedList(n));
List<int> _takeI16s(Pointer<Int16> p, int n) {
  final v = _copyI16s(p, n);
  _freeArray(p, n, 2);
  return v;
}

void _handOverI16s(
        List<int> v, Pointer<Pointer<Int16>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Int16List ? v : Int16List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Int32> _stageI32s(Arena a, List<int> v) =>
    _stageData(a, v is Int32List ? v : Int32List.fromList(v)).cast();
List<int> _copyI32s(Pointer<Int32> p, int n) =>
    n == 0 ? Int32List(0) : Int32List.fromList(p.asTypedList(n));
List<int> _takeI32s(Pointer<Int32> p, int n) {
  final v = _copyI32s(p, n);
  _freeArray(p, n, 4);
  return v;
}

void _handOverI32s(
        List<int> v, Pointer<Pointer<Int32>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Int32List ? v : Int32List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Int64> _stageI64s(Arena a, List<int> v) =>
    _stageData(a, v is Int64List ? v : Int64List.fromList(v)).cast();
List<int> _copyI64s(Pointer<Int64> p, int n) =>
    n == 0 ? Int64List(0) : Int64List.fromList(p.asTypedList(n));
List<int> _takeI64s(Pointer<Int64> p, int n) {
  final v = _copyI64s(p, n);
  _freeArray(p, n, 8);
  return v;
}

void _handOverI64s(
        List<int> v, Pointer<Pointer<Int64>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Int64List ? v : Int64List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Uint16> _stageU16s(Arena a, List<int> v) =>
    _stageData(a, v is Uint16List ? v : Uint16List.fromList(v)).cast();
List<int> _copyU16s(Pointer<Uint16> p, int n) =>
    n == 0 ? Uint16List(0) : Uint16List.fromList(p.asTypedList(n));
List<int> _takeU16s(Pointer<Uint16> p, int n) {
  final v = _copyU16s(p, n);
  _freeArray(p, n, 2);
  return v;
}

void _handOverU16s(
        List<int> v, Pointer<Pointer<Uint16>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Uint16List ? v : Uint16List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Uint32> _stageU32s(Arena a, List<int> v) =>
    _stageData(a, v is Uint32List ? v : Uint32List.fromList(v)).cast();
List<int> _copyU32s(Pointer<Uint32> p, int n) =>
    n == 0 ? Uint32List(0) : Uint32List.fromList(p.asTypedList(n));
List<int> _takeU32s(Pointer<Uint32> p, int n) {
  final v = _copyU32s(p, n);
  _freeArray(p, n, 4);
  return v;
}

void _handOverU32s(
        List<int> v, Pointer<Pointer<Uint32>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Uint32List ? v : Uint32List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Uint64> _stageU64s(Arena a, List<int> v) =>
    _stageData(a, v is Uint64List ? v : Uint64List.fromList(v)).cast();
List<int> _copyU64s(Pointer<Uint64> p, int n) =>
    n == 0 ? Uint64List(0) : Uint64List.fromList(p.asTypedList(n));
List<int> _takeU64s(Pointer<Uint64> p, int n) {
  final v = _copyU64s(p, n);
  _freeArray(p, n, 8);
  return v;
}

void _handOverU64s(
        List<int> v, Pointer<Pointer<Uint64>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Uint64List ? v : Uint64List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Float> _stageF32s(Arena a, List<double> v) =>
    _stageData(a, v is Float32List ? v : Float32List.fromList(v)).cast();
List<double> _copyF32s(Pointer<Float> p, int n) =>
    n == 0 ? Float32List(0) : Float32List.fromList(p.asTypedList(n));
List<double> _takeF32s(Pointer<Float> p, int n) {
  final v = _copyF32s(p, n);
  _freeArray(p, n, 4);
  return v;
}

void _handOverF32s(
        List<double> v, Pointer<Pointer<Float>> outPtr, Pointer<Size> outLen) =>
    _handOverData(v is Float32List ? v : Float32List.fromList(v), v.length,
        outPtr.cast(), outLen);

Pointer<Double> _stageF64s(Arena a, List<double> v) =>
    _stageData(a, v is Float64List ? v : Float64List.fromList(v)).cast();
List<double> _copyF64s(Pointer<Double> p, int n) =>
    n == 0 ? Float64List(0) : Float64List.fromList(p.asTypedList(n));
List<double> _takeF64s(Pointer<Double> p, int n) {
  final v = _copyF64s(p, n);
  _freeArray(p, n, 8);
  return v;
}

void _handOverF64s(List<double> v, Pointer<Pointer<Double>> outPtr,
        Pointer<Size> outLen) =>
    _handOverData(v is Float64List ? v : Float64List.fromList(v), v.length,
        outPtr.cast(), outLen);
