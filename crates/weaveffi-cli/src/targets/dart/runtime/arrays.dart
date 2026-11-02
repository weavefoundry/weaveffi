// ── Typed arrays ──
// A numeric list (`[i32]`, `[f64]`, ...) crosses as a run of its packed
// native elements, not as a value buffer; the run's length counts bytes. An
// argument is copied into arena memory for the call; a returned array is
// copied into a typed list (an `Int32List`, a `Float64List`, ...) and the
// run released with `{{PREFIX}}_run_free`; a callback's array argument is
// copied before the method runs; and a callback's returned array is handed
// over as a `{{PREFIX}}_alloc` run the producer adopts. A `u64` element
// travels as its two's-complement bit pattern, like every other `u64`.

/// Stages [data]'s bytes for one call.
_Run _stageData(Arena arena, TypedData data) =>
    _stage(arena, Uint8List.sublistView(data));

/// Hands [data]'s bytes to the producer as a `{{PREFIX}}_alloc` run.
_Run _handOverData(TypedData data) => _handOver(Uint8List.sublistView(data));

/// A run's elements, viewed in place (every run is aligned for its element).
Pointer<T> _elements<T extends NativeType>(_Run run) => run.ptr.cast<T>();

/// Copies an owned array and releases its run.
T _takeArray<T>(_Run run, T Function(_Run) copy) {
  try {
    return copy(run);
  } finally {
    _runFree(run);
  }
}

_Run _stageI8s(Arena a, List<int> v) =>
    _stageData(a, v is Int8List ? v : Int8List.fromList(v));
Int8List _copyI8s(_Run r) => r.len == 0
    ? Int8List(0)
    : Int8List.fromList(_elements<Int8>(r).asTypedList(r.len));
Int8List _takeI8s(_Run r) => _takeArray(r, _copyI8s);
_Run _handOverI8s(List<int> v) =>
    _handOverData(v is Int8List ? v : Int8List.fromList(v));

_Run _stageI16s(Arena a, List<int> v) =>
    _stageData(a, v is Int16List ? v : Int16List.fromList(v));
Int16List _copyI16s(_Run r) => r.len == 0
    ? Int16List(0)
    : Int16List.fromList(_elements<Int16>(r).asTypedList(r.len ~/ 2));
Int16List _takeI16s(_Run r) => _takeArray(r, _copyI16s);
_Run _handOverI16s(List<int> v) =>
    _handOverData(v is Int16List ? v : Int16List.fromList(v));

_Run _stageI32s(Arena a, List<int> v) =>
    _stageData(a, v is Int32List ? v : Int32List.fromList(v));
Int32List _copyI32s(_Run r) => r.len == 0
    ? Int32List(0)
    : Int32List.fromList(_elements<Int32>(r).asTypedList(r.len ~/ 4));
Int32List _takeI32s(_Run r) => _takeArray(r, _copyI32s);
_Run _handOverI32s(List<int> v) =>
    _handOverData(v is Int32List ? v : Int32List.fromList(v));

_Run _stageI64s(Arena a, List<int> v) =>
    _stageData(a, v is Int64List ? v : Int64List.fromList(v));
Int64List _copyI64s(_Run r) => r.len == 0
    ? Int64List(0)
    : Int64List.fromList(_elements<Int64>(r).asTypedList(r.len ~/ 8));
Int64List _takeI64s(_Run r) => _takeArray(r, _copyI64s);
_Run _handOverI64s(List<int> v) =>
    _handOverData(v is Int64List ? v : Int64List.fromList(v));

_Run _stageU16s(Arena a, List<int> v) =>
    _stageData(a, v is Uint16List ? v : Uint16List.fromList(v));
Uint16List _copyU16s(_Run r) => r.len == 0
    ? Uint16List(0)
    : Uint16List.fromList(_elements<Uint16>(r).asTypedList(r.len ~/ 2));
Uint16List _takeU16s(_Run r) => _takeArray(r, _copyU16s);
_Run _handOverU16s(List<int> v) =>
    _handOverData(v is Uint16List ? v : Uint16List.fromList(v));

_Run _stageU32s(Arena a, List<int> v) =>
    _stageData(a, v is Uint32List ? v : Uint32List.fromList(v));
Uint32List _copyU32s(_Run r) => r.len == 0
    ? Uint32List(0)
    : Uint32List.fromList(_elements<Uint32>(r).asTypedList(r.len ~/ 4));
Uint32List _takeU32s(_Run r) => _takeArray(r, _copyU32s);
_Run _handOverU32s(List<int> v) =>
    _handOverData(v is Uint32List ? v : Uint32List.fromList(v));

_Run _stageU64s(Arena a, List<int> v) =>
    _stageData(a, v is Uint64List ? v : Uint64List.fromList(v));
Uint64List _copyU64s(_Run r) => r.len == 0
    ? Uint64List(0)
    : Uint64List.fromList(_elements<Uint64>(r).asTypedList(r.len ~/ 8));
Uint64List _takeU64s(_Run r) => _takeArray(r, _copyU64s);
_Run _handOverU64s(List<int> v) =>
    _handOverData(v is Uint64List ? v : Uint64List.fromList(v));

_Run _stageF32s(Arena a, List<double> v) =>
    _stageData(a, v is Float32List ? v : Float32List.fromList(v));
Float32List _copyF32s(_Run r) => r.len == 0
    ? Float32List(0)
    : Float32List.fromList(_elements<Float>(r).asTypedList(r.len ~/ 4));
Float32List _takeF32s(_Run r) => _takeArray(r, _copyF32s);
_Run _handOverF32s(List<double> v) =>
    _handOverData(v is Float32List ? v : Float32List.fromList(v));

_Run _stageF64s(Arena a, List<double> v) =>
    _stageData(a, v is Float64List ? v : Float64List.fromList(v));
Float64List _copyF64s(_Run r) => r.len == 0
    ? Float64List(0)
    : Float64List.fromList(_elements<Double>(r).asTypedList(r.len ~/ 8));
Float64List _takeF64s(_Run r) => _takeArray(r, _copyF64s);
_Run _handOverF64s(List<double> v) =>
    _handOverData(v is Float64List ? v : Float64List.fromList(v));
