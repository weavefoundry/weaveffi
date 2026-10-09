// Conformance consumer: codec sample, Dart target.
//
// The shared-vector loop: for every vector the producer serves, decode it
// with the generated codec, pass it back to `checkVector` (which re-encodes
// it), and push each primitive vector's value through the matching
// direct-family `echo*` (u64 as its two's-complement bit pattern, floats
// compared bitwise). Then vectors built from literals (so a symmetric
// encode/decode bug can't hide), spot checks of decoded fields, the typed
// OutOfRangeException and its payload, malformed input rejected as
// marshalling failures (sent through raw `dart:ffi`, since the bindings only
// encode well-formed values), and object identity and reference counting
// through buffers. Ends by asserting the producer's leak counters are zero.

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:codec/codec.dart' as c;
import 'package:ffi/ffi.dart';

import 'support.dart';

const int i8Min = -0x80;
const int i16Min = -0x8000;
const int i32Min = -0x80000000;
const int i64Min = -0x8000000000000000;
const int i64Max = 0x7fffffffffffffff;

/// Release the tokens a decoded vector carries (only `Objects` has any).
void release(c.Vector v) {
  if (v is c.VectorObjects) {
    final h = v.value;
    h.primary.dispose();
    h.spare?.dispose();
    for (final t in h.many) {
      t.dispose();
    }
    for (final t in h.byName.values) {
      t.dispose();
    }
  }
}

/// The index of the vector named [name].
int find(int n, String name) {
  for (var i = 0; i < n; i++) {
    if (c.vectorName(i) == name) return i;
  }
  throw StateError('no vector named $name');
}

int f32Bits(double v) =>
    (ByteData(4)..setFloat32(0, v, Endian.little)).getUint32(0, Endian.little);

int f64Bits(double v) =>
    (ByteData(8)..setFloat64(0, v, Endian.little)).getUint64(0, Endian.little);

bool bytesEqual(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// Push a primitive vector's value through its direct-family echo.
void echo(c.Vector v) {
  switch (v) {
    case c.VectorI8(:final value):
      expect(c.echoI8(value) == value, 'echoI8 $value');
    case c.VectorU8(:final value):
      expect(c.echoU8(value) == value, 'echoU8 $value');
    case c.VectorI16(:final value):
      expect(c.echoI16(value) == value, 'echoI16 $value');
    case c.VectorU16(:final value):
      expect(c.echoU16(value) == value, 'echoU16 $value');
    case c.VectorI32(:final value):
      expect(c.echoI32(value) == value, 'echoI32 $value');
    case c.VectorU32(:final value):
      expect(c.echoU32(value) == value, 'echoU32 $value');
    case c.VectorI64(:final value):
      expect(c.echoI64(value) == value, 'echoI64 $value');
    case c.VectorU64(:final value):
      expect(c.echoU64(value) == value, 'echoU64 $value');
    case c.VectorF32(:final value):
      expect(f32Bits(c.echoF32(value)) == f32Bits(value), 'echoF32 $value');
    case c.VectorF64(:final value):
      expect(f64Bits(c.echoF64(value)) == f64Bits(value), 'echoF64 $value');
    case c.VectorFlag(:final value):
      expect(c.echoBool(value) == value, 'echoBool $value');
    case c.VectorHue(:final value):
      expect(c.echoColor(value) == value, 'echoColor $value');
    case c.VectorText(:final value):
      expect(c.echoText(value) == value, 'echoText');
    case c.VectorBlob(:final value):
      expect(bytesEqual(c.echoBlob(value), value), 'echoBlob');
    default:
      break;
  }
}

void everyVector(int n) {
  for (var i = 0; i < n; i++) {
    final v = c.vector(i);
    if (!c.checkVector(i, v)) {
      throw StateError(
        'vector $i (${c.vectorName(i)}) did not round-trip; '
        'producer saw ${c.describeVector(v)}',
      );
    }
    expect(
      !c.checkVector((i + 1) % n, v),
      'vector $i never matches its neighbor',
    );
    echo(v);
    release(v);
  }
}

c.Scalars canonicalScalars({int u16 = 60000}) => c.Scalars(
  i8Value: -8,
  u8Value: 200,
  i16Value: -16000,
  u16Value: u16,
  i32Value: -2000000000,
  u32Value: 4000000000,
  i64Value: -9007199254740993,
  u64Value: -1, // u64::MAX
  f32Value: 1.5,
  f64Value: -2.25e100,
  flag: true,
  color: c.Color.blue,
);

void literalVectors(int n) {
  final canonical = canonicalScalars();
  expect(
    c.checkVector(find(n, 'scalars canonical'), c.VectorAllScalars(canonical)),
    'scalars canonical',
  );
  expect(canonical == canonicalScalars(), 'records compare by value');
  expect(
    !c.checkVector(
      find(n, 'scalars canonical'),
      c.VectorAllScalars(canonicalScalars(u16: 60001)),
    ),
    'a changed field no longer matches',
  );
  expect(
    c.checkVector(
      find(n, 'shape labeled'),
      c.VectorFigure(c.ShapeLabeled('tag', 3)),
    ),
    'shape labeled',
  );
  expect(
    c.checkVector(
      find(n, 'string interior nul'),
      c.VectorText('nul\u0000inside\u0000'),
    ),
    'string interior nul',
  );
  // Any NaN matches the NaN vector; zero keeps its sign.
  final nan = (ByteData(8)..setUint64(0, 0x7ff8000000000001)).getFloat64(0);
  expect(
    c.checkVector(find(n, 'f64 nan'), c.VectorF64(nan)),
    'a NaN payload matches f64 nan',
  );
  expect(
    c.checkVector(find(n, 'f64 -0'), c.VectorF64(-0.0)),
    '-0.0 matches f64 -0',
  );
  expect(
    !c.checkVector(find(n, 'f64 -0'), c.VectorF64(0.0)),
    "+0.0 doesn't match f64 -0",
  );
  expect(c.checkVector(find(n, 'u64 max'), c.VectorU64(-1)), 'u64 max');
  expect(
    c.checkVector(find(n, 'enum infrared'), c.VectorHue(c.Color.infrared)),
    'enum infrared',
  );
  expect(
    c.checkVector(find(n, 'optional zero'), c.VectorMaybeI64(0)),
    'optional zero',
  );
  expect(
    c.checkVector(find(n, 'optional absent'), c.VectorMaybeI64(null)),
    'optional absent',
  );
  expect(
    !c.checkVector(find(n, 'optional zero'), c.VectorMaybeI64(null)),
    "absent isn't zero",
  );
  // A map's entry order doesn't matter on the wire.
  final counts = <String, int>{'x': 0, 'héllo': -1, '': i64Max};
  expect(
    c.checkVector(find(n, 'map of strings'), c.VectorCounts(counts)),
    'map of strings',
  );
  expect(c.checkVector(find(n, 'blank'), const c.VectorBlank()), 'blank');
  // A holder of a consumer-made token.
  final lone = c.Token(-1);
  final sparse = c.VectorObjects(
    c.Holder(primary: lone, many: const [], byName: const {}),
  );
  expect(c.checkVector(find(n, 'objects sparse'), sparse), 'objects sparse');
  lone.dispose();
}

void spotChecks(int n) {
  final past53 = c.vector(find(n, 'i64 past 2^53'));
  expect(
    past53 == c.VectorI64(-9007199254740993),
    'i64 past 2^53 is exact (got $past53)',
  );

  final subnormal = c.vector(find(n, 'f32 min subnormal'));
  expect(
    subnormal is c.VectorF32 && f32Bits(subnormal.value) == 1,
    'f32 min subnormal has bits 1',
  );

  expect(
    c.vector(find(n, 'string astral')) == c.VectorText('🦀 crab 😀'),
    'string astral',
  );

  final minimum = c.vector(find(n, 'scalars minimum'));
  expect(minimum is c.VectorAllScalars, 'scalars minimum is AllScalars');
  final m = (minimum as c.VectorAllScalars).value;
  expect(m.i8Value == i8Min && m.i16Value == i16Min, 'scalars minimum i8/i16');
  expect(
    m.i32Value == i32Min && m.i64Value == i64Min,
    'scalars minimum i32/i64',
  );
  expect(m.u8Value == 0 && m.u64Value == 0, 'scalars minimum unsigned');
  expect(
    m.f32Value == double.negativeInfinity && m.f64Value.isNaN,
    'scalars minimum floats',
  );
  expect(
    m.color == c.Color.infrared && !m.flag,
    'scalars minimum color and flag',
  );

  final deep = c.vector(find(n, 'composite canonical'));
  expect(deep is c.VectorDeep, 'composite canonical is Deep');
  final d = (deep as c.VectorDeep).value;
  expect(d.name == 'héllo wörld ✓', 'composite name (got ${d.name})');
  expect(d.blob.length == 6 && d.blob[5] == 255, 'composite blob');
  expect(d.someI64 == i64Min && d.noneI64 == null, 'composite optionals');
  expect(d.someText == '', 'composite someText is present and empty');
  expect(d.names.length == 3 && d.names[1] == '', 'composite names');
  expect(
    d.matrix.length == 3 && d.matrix[1].isEmpty && d.matrix[2][0] == -4,
    'composite matrix',
  );
  expect(
    d.floats.length == 6 && d.floats[0].isNaN && d.floats[3].isNegative,
    'composite floats',
  );
  expect(
    d.byName.length == 4 &&
        d.byId.length == 3 &&
        d.byColor.length == 2 &&
        d.flags.length == 2,
    'composite maps',
  );
  expect(d.scalars.u32Value == 4000000000, 'composite scalars.u32Value');
  final shape = d.shape;
  expect(shape is c.ShapeLabeled && shape.count == 3, 'composite shape');
  final last = d.shapes.last;
  expect(
    d.shapes.length == 6 && last is c.ShapeNested && last.note == null,
    'composite shapes',
  );
  expect(d.maybeShape is c.ShapeNested, 'composite maybeShape');
  expect(d.maybeList?.length == 2, 'composite maybeList');
  expect(
    d.sparse.length == 3 && d.sparse[0] == true && d.sparse[1] == null,
    'composite sparse',
  );
  expect(
    d.colors.length == 4 && d.colors[3] == c.Color.infrared,
    'composite colors',
  );
  // Decoding the same vector twice gives equal (but distinct) values.
  final again = c.vector(find(n, 'composite canonical'));
  expect(
    !identical(again, deep) && again.hashCode == deep.hashCode,
    'composites hash by value',
  );
}

void outOfRange(int n) {
  final e = expectThrows<c.OutOfRangeException>(
    () => c.vector(n),
    'vector(n) raises OutOfRange',
  );
  expect(
    e.code == 1 && e.index == n && e.count == n,
    'OutOfRange payload (index ${e.index}, count ${e.count})',
  );
  expect(
    e.message == 'vector $n is out of range (count $n)',
    'OutOfRange message (got ${e.message})',
  );

  final e2 = expectThrows<c.OutOfRangeException>(
    () => c.vectorName(n + 5),
    'vectorName(n + 5) raises OutOfRange',
  );
  expect(e2.index == n + 5 && e2.count == n, 'vectorName OutOfRange payload');

  expect(
    !c.checkVector(n, const c.VectorBlank()),
    'checkVector past the end is false',
  );
}

/// The C `codec_error` struct, for raw calls.
final class RawError extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
  external Pointer<Uint8> payloadPtr;
  @Size()
  external int payloadLen;
}

/// The producer's symbols, bound directly so malformed input can be sent.
final class Raw {
  Raw() : _lib = DynamicLibrary.open(Platform.environment['CODEC_LIBRARY']!);

  final DynamicLibrary _lib;

  late final checkVector = _lib
      .lookupFunction<
        Bool Function(Uint32, Pointer<Uint8>, Size, Pointer<RawError>),
        bool Function(int, Pointer<Uint8>, int, Pointer<RawError>)
      >('codec_codec_check_vector');
  late final echoColor = _lib
      .lookupFunction<
        Int32 Function(Int32, Pointer<RawError>),
        int Function(int, Pointer<RawError>)
      >('codec_codec_echo_color');
  late final echoText = _lib
      .lookupFunction<
        Pointer<Uint8> Function(
          Pointer<Uint8>,
          Size,
          Pointer<Size>,
          Pointer<RawError>,
        ),
        Pointer<Uint8> Function(
          Pointer<Uint8>,
          int,
          Pointer<Size>,
          Pointer<RawError>,
        )
      >('codec_codec_echo_text');
  late final sumHolder = _lib
      .lookupFunction<
        Int64 Function(Pointer<Uint8>, Size, Pointer<RawError>),
        int Function(Pointer<Uint8>, int, Pointer<RawError>)
      >('codec_codec_sum_holder');
  late final errorClear = _lib
      .lookupFunction<
        Void Function(Pointer<RawError>),
        void Function(Pointer<RawError>)
      >('codec_error_clear');

  /// Runs [call] with a staged copy of [bytes] and a fresh error slot, and
  /// returns the error code it reported.
  int code(
    List<int> bytes,
    void Function(Pointer<Uint8>, int, Pointer<RawError>) call,
  ) {
    final ptr = calloc<Uint8>(bytes.length + 1);
    final err = calloc<RawError>();
    try {
      ptr.asTypedList(bytes.length).setAll(0, bytes);
      call(ptr, bytes.length, err);
      final code = err.ref.code;
      errorClear(err);
      return code;
    } finally {
      calloc.free(ptr);
      calloc.free(err);
    }
  }
}

/// Little-endian buffer bytes for the malformed vectors.
List<int> i32(int v) =>
    (ByteData(4)..setInt32(0, v, Endian.little)).buffer.asUint8List();
List<int> u32(int v) =>
    (ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List();
List<int> i64(int v) =>
    (ByteData(8)..setInt64(0, v, Endian.little)).buffer.asUint8List();
List<int> str(String s) => [...u32(utf8.encode(s).length), ...utf8.encode(s)];

void malformed(Raw raw) {
  // Tags are the declaration order of the Vector variants.
  const tagI64 = 7, tagFlag = 11, tagText = 12, tagHue = 14, tagCounts = 19;
  final cases = <String, List<int>>{
    'a truncated buffer': [...i32(tagI64), ...u32(7)],
    'an unknown tag': i32(999),
    'trailing bytes': [...i32(0), 0],
    'a bool that is neither 0 nor 1': [...i32(tagFlag), 2],
    'an undeclared enum value': [...i32(tagHue), ...i32(3)],
    'a repeated map key': [
      ...i32(tagCounts),
      ...u32(2),
      ...str('a'),
      ...i64(1),
      ...str('a'),
      ...i64(2),
    ],
    "a string that isn't UTF-8": [...i32(tagText), ...u32(2), 0xC3, 0x28],
  };
  for (final MapEntry(key: what, value: bytes) in cases.entries) {
    final code = raw.code(bytes, (p, n, e) => raw.checkVector(0, p, n, e));
    expect(code == -3, '$what is rejected with -3 (got $code)');
  }
  // Color can't spell an undeclared value, but a raw call can.
  final colorCode = raw.code(const [], (_, __, e) => raw.echoColor(3, e));
  expect(colorCode == -3, 'an undeclared Color is rejected (got $colorCode)');
  final len = calloc<Size>();
  final textCode = raw.code([
    0xC3,
    0x28,
  ], (p, n, e) => raw.echoText(p, n, len, e));
  calloc.free(len);
  expect(textCode == -3, 'invalid UTF-8 is rejected (got $textCode)');
  // A zero token is a marshalling failure, not a crash.
  final zeroCode = raw.code(
    List<int>.filled(17, 0),
    (p, n, e) => raw.sumHolder(p, n, e),
  );
  expect(zeroCode == -3, 'a zero token is rejected (got $zeroCode)');
}

void objects(int n) {
  final full = c.vector(find(n, 'objects full'));
  expect(full is c.VectorObjects, 'objects full is Objects');
  final h = (full as c.VectorObjects).value;
  expect(
    h.primary.value() == 10 && h.spare?.value() == 11,
    'holder primary and spare',
  );
  expect(
    bytesEqual([for (final t in h.many) t.value()], [12, 13, i64Min]),
    'holder many',
  );
  expect(
    h.byName.length == 2 &&
        h.byName['a']!.value() == 20 &&
        h.byName['b']!.value() == 21,
    'holder byName',
  );
  // Each encoding mints fresh references, so the holder can be sent twice.
  const expected = 10 + 11 + 12 + 13 + 20 + 21 + i64Min;
  expect(
    c.sumHolder(h) == expected && c.sumHolder(h) == expected,
    'sumHolder twice',
  );

  // primaryOf returns the very same object (a new reference to it).
  final p = c.primaryOf(h);
  expect(p.value() == 10, 'primaryOf value');
  expect(
    c.samePrimary(h, c.Holder(primary: p, many: const [], byName: const {})),
    'primaryOf is the same object',
  );
  final twin = c.Token(10);
  expect(
    !c.samePrimary(
      h,
      c.Holder(primary: twin, many: const [], byName: const {}),
    ),
    "an equal value isn't the same object",
  );

  // A holder built from consumer tokens, the same wrapper in several slots.
  final minus4 = c.Token(-4);
  final mine = c.Holder(
    primary: twin,
    spare: twin,
    many: [twin, twin, minus4],
    byName: {'k': twin},
  );
  expect(c.sumHolder(mine) == 10 * 5 - 4, 'consumer holder sums to 46');

  minus4.dispose();
  twin.dispose();
  twin.dispose(); // disposing twice is safe
  p.dispose();
  release(full);
  expectThrows<StateError>(() => p.value(), "a disposed wrapper can't be used");
}

Future<void> main() async {
  final n = c.vectorCount();
  expect(n >= 60, 'at least 60 vectors (got $n)');

  everyVector(n);
  literalVectors(n);
  spotChecks(n);
  outOfRange(n);
  malformed(Raw());
  objects(n);

  await expectNoLeaks('codec');
  print('dart/codec: OK ($n vectors)');
}
