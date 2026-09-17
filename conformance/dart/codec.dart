// Conformance consumer: codec sample, Dart target.
//
// Round-trips every value-buffer wire shape through the producer oracle in
// both directions. `sample*` proves the Dart decoder reads exactly what Rust
// encoded (checked field by field against the canonical fixture and by
// handing the value back to `verify*`); `roundtrip*` proves the Dart encoder
// matches (the producer decodes, re-encodes, and the result is compared
// field by field); Dart-built values with edge cases (empty strings, lists,
// and maps, non-ASCII and NUL-bearing text, i64/u64 extremes, NaN, the
// infinities, negative zero, f32 rounding) cover what the fixture does not;
// `Shape` exercises every rich-enum variant; `Holder` exercises object
// tokens inside records, optionals, and lists (each encoding mints a cloned
// reference, `primaryOf` returns the same object, wrappers stay usable and
// dispose cleanly, double dispose is safe). Dart ints are signed 64-bit, so
// the generator carries `u64` as its two's-complement bit pattern: u64::MAX
// is -1. Throws (non-zero exit) on any mismatch.

import 'dart:typed_data';

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

const int i64Min = -9223372036854775808;
const int i64Max = 9223372036854775807;
// u64::MAX and 2^63 as Dart's signed 64-bit bit patterns.
const int u64Max = -1;
const int u64High = i64Min;

/// The value an f64 takes after passing through an f32 slot.
double f32(double v) => (Float32List(1)..[0] = v)[0];

bool sameDouble(double a, double b) {
  if (a.isNaN || b.isNaN) return a.isNaN && b.isNaN;
  return a == b && a.isNegative == b.isNegative;
}

bool listEq<T>(List<T> a, List<T> b, [bool Function(T, T)? eq]) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (!(eq?.call(a[i], b[i]) ?? a[i] == b[i])) return false;
  }
  return true;
}

bool scalarsEq(wv.Scalars a, wv.Scalars b) =>
    a.i8Value == b.i8Value &&
    a.u8Value == b.u8Value &&
    a.i16Value == b.i16Value &&
    a.u16Value == b.u16Value &&
    a.i32Value == b.i32Value &&
    a.u32Value == b.u32Value &&
    a.i64Value == b.i64Value &&
    a.u64Value == b.u64Value &&
    sameDouble(a.f32Value, b.f32Value) &&
    sameDouble(a.f64Value, b.f64Value) &&
    a.flag == b.flag &&
    a.color == b.color;

bool shapeEq(wv.Shape a, wv.Shape b) {
  switch ((a, b)) {
    case (wv.ShapeEmpty(), wv.ShapeEmpty()):
      return true;
    case (wv.ShapeCircle x, wv.ShapeCircle y):
      return sameDouble(x.radius, y.radius);
    case (wv.ShapeRect x, wv.ShapeRect y):
      return sameDouble(x.width, y.width) && sameDouble(x.height, y.height);
    case (wv.ShapeLabeled x, wv.ShapeLabeled y):
      return x.label == y.label && x.count == y.count;
    case (wv.ShapeNested x, wv.ShapeNested y):
      return scalarsEq(x.inner, y.inner) && x.note == y.note;
    default:
      return false;
  }
}

bool mapEq<K, V>(Map<K, V> a, Map<K, V> b, [bool Function(V, V)? eq]) {
  if (a.length != b.length) return false;
  for (final e in a.entries) {
    if (!b.containsKey(e.key)) return false;
    final other = b[e.key] as V;
    if (!(eq?.call(e.value, other) ?? e.value == other)) return false;
  }
  return true;
}

bool compositeEq(wv.Composite a, wv.Composite b) =>
    a.name == b.name &&
    listEq(a.blob, b.blob) &&
    a.someI64 == b.someI64 &&
    a.noneI64 == b.noneI64 &&
    a.someText == b.someText &&
    listEq(a.names, b.names) &&
    listEq(a.matrix, b.matrix, (x, y) => listEq(x, y)) &&
    listEq(a.empty, b.empty, sameDouble) &&
    mapEq(a.byName, b.byName) &&
    mapEq(a.byId, b.byId, scalarsEq) &&
    scalarsEq(a.scalars, b.scalars) &&
    shapeEq(a.shape, b.shape) &&
    listEq(a.shapes, b.shapes, shapeEq) &&
    ((a.maybeShape == null && b.maybeShape == null) ||
        (a.maybeShape != null &&
            b.maybeShape != null &&
            shapeEq(a.maybeShape!, b.maybeShape!))) &&
    ((a.maybeList == null && b.maybeList == null) ||
        (a.maybeList != null &&
            b.maybeList != null &&
            listEq(a.maybeList!, b.maybeList!))) &&
    listEq(a.sparse, b.sparse) &&
    listEq(a.colors, b.colors);

wv.Scalars canonicalScalars({bool flag = true}) => wv.Scalars(
      i8Value: -8,
      u8Value: 200,
      i16Value: -16000,
      u16Value: 60000,
      i32Value: -2000000000,
      u32Value: 4000000000,
      i64Value: -9007199254740993,
      u64Value: u64Max,
      f32Value: 1.5,
      f64Value: -2.25e100,
      flag: flag,
      color: wv.Color.blue,
    );

wv.Composite canonicalComposite() => wv.Composite(
      name: 'héllo wörld ✓',
      blob: <int>[0, 1, 2, 253, 254, 255],
      someI64: i64Min,
      noneI64: null,
      someText: '',
      names: <String>['a', '', 'ccc'],
      matrix: <List<int>>[
        <int>[1, 2, 3],
        <int>[],
        <int>[-4]
      ],
      empty: <double>[],
      byName: <String, int>{'one': 1, 'two': 2, 'neg': -3},
      byId: <int, wv.Scalars>{
        -1: canonicalScalars(),
        42: canonicalScalars(flag: false),
      },
      scalars: canonicalScalars(),
      shape: wv.ShapeLabeled('tag', 3),
      shapes: <wv.Shape>[
        wv.ShapeEmpty(),
        wv.ShapeCircle(2.5),
        wv.ShapeRect(1.0, 0.5),
        wv.ShapeLabeled('', -1),
        wv.ShapeNested(canonicalScalars(), 'n'),
      ],
      maybeShape: wv.ShapeNested(canonicalScalars(), null),
      maybeList: <int>[9, 8],
      sparse: <bool?>[true, null, false],
      colors: <wv.Color>[wv.Color.red, wv.Color.green, wv.Color.blue],
    );

void checkScalars() {
  // Producer encodes, Dart decodes: every field of the canonical fixture.
  final s = wv.sampleScalars();
  expect(s.i8Value == -8, 'i8 (got ${s.i8Value})');
  expect(s.u8Value == 200, 'u8 (got ${s.u8Value})');
  expect(s.i16Value == -16000, 'i16 (got ${s.i16Value})');
  expect(s.u16Value == 60000, 'u16 (got ${s.u16Value})');
  expect(s.i32Value == -2000000000, 'i32 (got ${s.i32Value})');
  expect(s.u32Value == 4000000000, 'u32 (got ${s.u32Value})');
  expect(s.i64Value == -9007199254740993, 'i64 (got ${s.i64Value})');
  expect(s.u64Value == u64Max, 'u64::MAX arrives as -1 (got ${s.u64Value})');
  expect(s.f32Value == 1.5, 'f32 (got ${s.f32Value})');
  expect(s.f64Value == -2.25e100, 'f64 (got ${s.f64Value})');
  expect(s.flag, 'flag');
  expect(s.color == wv.Color.blue, 'color blue');
  expect(scalarsEq(s, canonicalScalars()), 'sample equals the Dart fixture');

  // Dart encodes, producer decodes.
  expect(wv.verifyScalars(s), 'verifyScalars(sample)');
  expect(wv.verifyScalars(canonicalScalars()), 'verifyScalars(Dart-built)');
  expect(scalarsEq(wv.roundtripScalars(s), s), 'roundtripScalars(sample)');

  // A mismatch is the typed domain error.
  final tweaked = wv.Scalars(
    i8Value: s.i8Value,
    u8Value: s.u8Value,
    i16Value: s.i16Value,
    u16Value: s.u16Value,
    i32Value: s.i32Value,
    u32Value: s.u32Value,
    i64Value: s.i64Value,
    u64Value: s.u64Value,
    f32Value: s.f32Value,
    f64Value: s.f64Value,
    flag: false,
    color: s.color,
  );
  try {
    wv.verifyScalars(tweaked);
    throw StateError('expected MismatchException');
  } on wv.MismatchException catch (e) {
    expect(e.code == 1, 'Mismatch code == 1 (got ${e.code})');
    expect(e.message == 'value does not match the canonical fixture',
        'Mismatch message (got ${e.message})');
    expect(e is wv.CodecException && e is wv.WeaveFFIException,
        'Mismatch extends the domain and brand exceptions');
  }

  // Edge values in every slot: signed and unsigned extremes, u64 above 2^63,
  // negative zero in f32, NaN in f64.
  final edges = wv.Scalars(
    i8Value: -128,
    u8Value: 255,
    i16Value: -32768,
    u16Value: 65535,
    i32Value: -2147483648,
    u32Value: 4294967295,
    i64Value: i64Min,
    u64Value: u64High,
    f32Value: -0.0,
    f64Value: double.nan,
    flag: false,
    color: wv.Color.red,
  );
  final back = wv.roundtripScalars(edges);
  expect(scalarsEq(back, edges), 'roundtripScalars(edges)');
  expect(back.f32Value == 0 && back.f32Value.isNegative, 'f32 -0.0 kept');
  expect(back.f64Value.isNaN, 'f64 NaN kept');
  expect(back.u64Value == u64High, 'u64 2^63 bit pattern kept');
  final highs = wv.Scalars(
    i8Value: 127,
    u8Value: 0,
    i16Value: 32767,
    u16Value: 0,
    i32Value: 2147483647,
    u32Value: 0,
    i64Value: i64Max,
    u64Value: 0,
    f32Value: f32(0.1),
    f64Value: double.negativeInfinity,
    flag: true,
    color: wv.Color.green,
  );
  final back2 = wv.roundtripScalars(highs);
  expect(scalarsEq(back2, highs), 'roundtripScalars(highs)');
  expect(back2.f32Value == f32(0.1), 'f32 0.1 rounds once');
  expect(back2.f64Value == double.negativeInfinity, 'f64 -inf kept');
}

void checkComposite() {
  final c = wv.sampleComposite();
  expect(c.name == 'héllo wörld ✓', 'name (got ${c.name})');
  expect(listEq(c.blob, <int>[0, 1, 2, 253, 254, 255]), 'blob (got ${c.blob})');
  expect(c.someI64 == i64Min, 'someI64 == i64::MIN (got ${c.someI64})');
  expect(c.noneI64 == null, 'noneI64 absent');
  expect(c.someText == '', 'someText present and empty (got ${c.someText})');
  expect(listEq(c.names, <String>['a', '', 'ccc']), 'names (got ${c.names})');
  expect(c.matrix.length == 3 &&
          listEq(c.matrix[0], <int>[1, 2, 3]) &&
          c.matrix[1].isEmpty &&
          listEq(c.matrix[2], <int>[-4]),
      'matrix (got ${c.matrix})');
  expect(c.empty.isEmpty, 'empty list');
  expect(c.byName.length == 3 &&
          c.byName['one'] == 1 &&
          c.byName['two'] == 2 &&
          c.byName['neg'] == -3,
      'byName (got ${c.byName})');
  expect(c.byId.length == 2, 'byId size');
  expect(scalarsEq(c.byId[-1]!, canonicalScalars()), 'byId[-1]');
  expect(scalarsEq(c.byId[42]!, canonicalScalars(flag: false)), 'byId[42]');
  expect(scalarsEq(c.scalars, canonicalScalars()), 'nested scalars');
  expect(c.shape is wv.ShapeLabeled, 'shape variant');
  final labeled = c.shape as wv.ShapeLabeled;
  expect(labeled.label == 'tag' && labeled.count == 3, 'shape fields');
  expect(c.shapes.length == 5, 'shapes length');
  expect(c.shapes[0] is wv.ShapeEmpty, 'shapes[0] Empty');
  expect((c.shapes[1] as wv.ShapeCircle).radius == 2.5, 'shapes[1] Circle');
  final rect = c.shapes[2] as wv.ShapeRect;
  expect(rect.width == 1.0 && rect.height == 0.5, 'shapes[2] Rect');
  final lab = c.shapes[3] as wv.ShapeLabeled;
  expect(lab.label == '' && lab.count == -1, 'shapes[3] Labeled');
  final nested = c.shapes[4] as wv.ShapeNested;
  expect(scalarsEq(nested.inner, canonicalScalars()) && nested.note == 'n',
      'shapes[4] Nested');
  final maybe = c.maybeShape;
  expect(maybe is wv.ShapeNested && maybe.note == null,
      'maybeShape Nested with absent note');
  expect(listEq(c.maybeList!, <int>[9, 8]), 'maybeList (got ${c.maybeList})');
  expect(c.sparse.length == 3 &&
          c.sparse[0] == true &&
          c.sparse[1] == null &&
          c.sparse[2] == false,
      'sparse (got ${c.sparse})');
  expect(listEq(c.colors, <wv.Color>[wv.Color.red, wv.Color.green, wv.Color.blue]),
      'colors (got ${c.colors})');
  expect(compositeEq(c, canonicalComposite()), 'sample equals the Dart fixture');

  // Dart encodes, producer decodes and compares.
  expect(wv.verifyComposite(c), 'verifyComposite(sample)');
  expect(wv.verifyComposite(canonicalComposite()),
      'verifyComposite(Dart-built)');
  expect(compositeEq(wv.roundtripComposite(c), c), 'roundtripComposite');
  final text = wv.describeComposite(c);
  expect(text.contains('héllo wörld ✓') && text.contains('Labeled'),
      'describeComposite renders the value (got $text)');

  // A one-element change deep inside is a Mismatch.
  final changed = wv.Composite(
    name: c.name,
    blob: c.blob,
    someI64: c.someI64,
    noneI64: c.noneI64,
    someText: c.someText,
    names: c.names,
    matrix: c.matrix,
    empty: c.empty,
    byName: c.byName,
    byId: c.byId,
    scalars: c.scalars,
    shape: c.shape,
    shapes: c.shapes,
    maybeShape: c.maybeShape,
    maybeList: c.maybeList,
    sparse: <bool?>[true, true, false],
    colors: c.colors,
  );
  try {
    wv.verifyComposite(changed);
    throw StateError('expected MismatchException for changed composite');
  } on wv.MismatchException catch (e) {
    expect(e.code == 1, 'composite Mismatch code');
  }

  // A Dart-built composite full of edge cases: empty everything, NUL and
  // astral-plane text, special floats, 32/64-bit extremes, absent optionals.
  final edge = wv.Composite(
    name: 'nul\u0000inside \u{1F600} end',
    blob: <int>[],
    someI64: null,
    noneI64: i64Max,
    someText: null,
    names: <String>[],
    matrix: <List<int>>[<int>[], <int>[-2147483648, 2147483647]],
    empty: <double>[
      double.nan,
      double.infinity,
      double.negativeInfinity,
      -0.0,
      5e-324,
      1.7976931348623157e308,
    ],
    byName: <String, int>{'': i64Min, 'max': i64Max, 'z': 0},
    byId: <int, wv.Scalars>{
      2147483647: canonicalScalars(flag: false),
      -2147483648: canonicalScalars(),
      0: canonicalScalars(),
    },
    scalars: canonicalScalars(),
    shape: wv.ShapeEmpty(),
    shapes: <wv.Shape>[],
    maybeShape: null,
    maybeList: null,
    sparse: <bool?>[null, null],
    colors: <wv.Color>[],
  );
  final edgeBack = wv.roundtripComposite(edge);
  expect(compositeEq(edgeBack, edge), 'roundtripComposite(edge)');
  expect(edgeBack.name.length == edge.name.length &&
          edgeBack.name.codeUnitAt(3) == 0,
      'NUL survives inside a length-prefixed string');
  expect(edgeBack.empty[0].isNaN, 'NaN in a list');
  expect(edgeBack.empty[3] == 0 && edgeBack.empty[3].isNegative,
      'negative zero in a list');
  expect(edgeBack.byName[''] == i64Min, 'empty-string key with i64::MIN');
  expect(
      listEq(edgeBack.byId.keys.toList()..sort(),
          <int>[-2147483648, 0, 2147483647]),
      'i32 map keys (got ${edgeBack.byId.keys})');
  expect(edgeBack.maybeShape == null && edgeBack.maybeList == null,
      'absent optionals stay absent');
  expect(edgeBack.someI64 == null && edgeBack.noneI64 == i64Max,
      'optional i64 both ways');

  // Fully empty composite.
  final bare = wv.Composite(
    name: '',
    blob: <int>[],
    names: <String>[],
    matrix: <List<int>>[],
    empty: <double>[],
    byName: <String, int>{},
    byId: <int, wv.Scalars>{},
    scalars: canonicalScalars(),
    shape: wv.ShapeEmpty(),
    shapes: <wv.Shape>[],
    sparse: <bool?>[],
    colors: <wv.Color>[],
  );
  expect(compositeEq(wv.roundtripComposite(bare), bare),
      'roundtripComposite(bare)');
}

void checkShapes() {
  final variants = <wv.Shape>[
    wv.ShapeEmpty(),
    wv.ShapeCircle(2.5),
    wv.ShapeCircle(double.nan),
    wv.ShapeCircle(-0.0),
    wv.ShapeRect(1.0, 0.5),
    wv.ShapeRect(f32(0.1), double.infinity),
    wv.ShapeLabeled('tag', 3),
    wv.ShapeLabeled('', -2147483648),
    wv.ShapeLabeled('ünïcödé ✓', 2147483647),
    wv.ShapeNested(canonicalScalars(), 'note'),
    wv.ShapeNested(canonicalScalars(flag: false), null),
  ];
  for (final v in variants) {
    final back = wv.roundtripShape(v);
    expect(back.runtimeType == v.runtimeType,
        'roundtripShape variant ${v.runtimeType} (got ${back.runtimeType})');
    expect(shapeEq(back, v), 'roundtripShape fields for ${v.runtimeType}');
  }
  final all = wv.roundtripShapes(variants);
  expect(listEq(all, variants, shapeEq), 'roundtripShapes list');
  expect(wv.roundtripShapes(<wv.Shape>[]).isEmpty, 'roundtripShapes empty');

  // describeShape renders the Rust Debug form, so the tag and payload the
  // producer actually decoded are visible.
  expect(wv.describeShape(wv.ShapeEmpty()) == 'Empty',
      'describe Empty (got ${wv.describeShape(wv.ShapeEmpty())})');
  expect(wv.describeShape(wv.ShapeCircle(2.5)) == 'Circle { radius: 2.5 }',
      'describe Circle (got ${wv.describeShape(wv.ShapeCircle(2.5))})');
  expect(
      wv.describeShape(wv.ShapeRect(1.0, 0.5)) ==
          'Rect { width: 1.0, height: 0.5 }',
      'describe Rect (got ${wv.describeShape(wv.ShapeRect(1.0, 0.5))})');
  expect(
      wv.describeShape(wv.ShapeLabeled('tag', 3)) ==
          'Labeled { label: "tag", count: 3 }',
      'describe Labeled (got ${wv.describeShape(wv.ShapeLabeled('tag', 3))})');
  final nestedText = wv.describeShape(wv.ShapeNested(canonicalScalars(), null));
  expect(
      nestedText.startsWith('Nested { inner: Scalars {') &&
          nestedText.contains('u64_value: 18446744073709551615') &&
          nestedText.endsWith('note: None }'),
      'describe Nested shows u64::MAX from the -1 bit pattern (got $nestedText)');
}

void checkDirect() {
  // Optionals, maps, strings, and bytes as top-level buffered or pointer
  // values.
  expect(wv.roundtripOptI64(null) == null, 'opt i64 null');
  expect(wv.roundtripOptI64(0) == 0, 'opt i64 zero');
  expect(wv.roundtripOptI64(i64Min) == i64Min, 'opt i64 min');
  expect(wv.roundtripOptI64(i64Max) == i64Max, 'opt i64 max');
  expect(wv.roundtripMap(<String, int>{}).isEmpty, 'empty map');
  final m = wv.roundtripMap(<String, int>{'': i64Min, 'k': -1, 'ü': i64Max});
  expect(m.length == 3 && m[''] == i64Min && m['k'] == -1 && m['ü'] == i64Max,
      'map values (got $m)');
  expect(wv.roundtripString('') == '', 'empty string');
  expect(wv.roundtripString('héllo wörld ✓ \u{1F600}') ==
          'héllo wörld ✓ \u{1F600}',
      'unicode string');
  expect(wv.roundtripBytes(<int>[]).isEmpty, 'empty bytes');
  expect(listEq(wv.roundtripBytes(<int>[0, 127, 128, 255]),
          <int>[0, 127, 128, 255]),
      'bytes');

  // 64-bit extremes by value; u64 above 2^63 travels as its bit pattern.
  expect(wv.roundtripI64(i64Min) == i64Min, 'i64 min');
  expect(wv.roundtripI64(i64Max) == i64Max, 'i64 max');
  expect(wv.roundtripI64(0) == 0, 'i64 zero');
  expect(wv.roundtripU64(0) == 0, 'u64 zero');
  expect(wv.roundtripU64(i64Max) == i64Max, 'u64 2^63-1');
  expect(wv.roundtripU64(u64High) == u64High, 'u64 2^63');
  expect(wv.roundtripU64(u64Max) == u64Max, 'u64 max');

  // Floats: NaN, infinities, negative zero, subnormal, and the largest finite.
  expect(wv.roundtripF64(double.nan).isNaN, 'f64 NaN');
  expect(wv.roundtripF64(double.infinity) == double.infinity, 'f64 +inf');
  expect(wv.roundtripF64(double.negativeInfinity) == double.negativeInfinity,
      'f64 -inf');
  final negZero = wv.roundtripF64(-0.0);
  expect(negZero == 0 && negZero.isNegative, 'f64 -0.0');
  expect(wv.roundtripF64(5e-324) == 5e-324, 'f64 subnormal');
  expect(wv.roundtripF64(1.7976931348623157e308) == 1.7976931348623157e308,
      'f64 max');
  expect(wv.roundtripF64(0.1) == 0.1, 'f64 0.1 exact');

  expect(wv.roundtripBool(true) && !wv.roundtripBool(false), 'bool');
  for (final c in wv.Color.values) {
    expect(wv.roundtripColor(c) == c, 'color $c');
  }
  expect(wv.Color.blue.value == 7 && wv.Color.fromValue(7) == wv.Color.blue,
      'Color non-contiguous discriminant');
}

void checkHolder() {
  // Producer-built holder: object tokens in a field, an optional, and a list
  // each decode into their own wrapper holding one reference.
  final h = wv.makeHolder(10, true);
  expect(h.primary.value() == 10, 'primary value');
  expect(h.spare != null && h.spare!.value() == 11, 'spare value');
  expect(h.many.length == 3, 'many length');
  expect(listEq(h.many.map((t) => t.value()).toList(), <int>[12, 13, 14]),
      'many values');

  // Each encoding clones a fresh reference per token, so the same holder can
  // be encoded repeatedly and every wrapper stays valid.
  expect(wv.sumHolder(h) == 60, 'sumHolder (got ${wv.sumHolder(h)})');
  expect(wv.sumHolder(h) == 60, 'sumHolder again');
  expect(h.primary.value() == 10 && h.many[2].value() == 14,
      'wrappers alive after encoding');

  // primaryOf returns the SAME object as h.primary: the producer compares
  // Arc identity, so a holder built around the returned wrapper matches,
  // while a fresh Token with the same value does not.
  final p = wv.primaryOf(h);
  expect(p.value() == 10, 'primaryOf value');
  final viaPrimaryOf = wv.Holder(primary: p, spare: null, many: <wv.Token>[]);
  expect(wv.samePrimary(h, viaPrimaryOf), 'primaryOf is the same object');
  expect(wv.samePrimary(h, h), 'samePrimary reflexive');
  final lookalike = wv.Token(10);
  final other =
      wv.Holder(primary: lookalike, spare: null, many: <wv.Token>[]);
  expect(!wv.samePrimary(h, other), 'equal value, different object');
  expect(wv.sumHolder(viaPrimaryOf) == 10, 'sum of a Dart-built holder');

  // Absent optional object.
  final bare = wv.makeHolder(0, false);
  expect(bare.spare == null, 'spare absent');
  expect(wv.sumHolder(bare) == 0 + 2 + 3 + 4, 'sum without spare');
  expect(bare.primary.value() == 0, 'zero-valued token');

  // Dart-built holder with Dart-created tokens, one of them repeated in every
  // position: every occurrence is a separate cloned reference.
  final t = wv.Token(5);
  final repeated = wv.Holder(primary: t, spare: t, many: <wv.Token>[t, t, t]);
  expect(wv.sumHolder(repeated) == 25, 'repeated token summed 5 times');
  expect(wv.samePrimary(repeated, repeated), 'repeated reflexive');
  final mixed = wv.Holder(
      primary: wv.Token(-1), spare: wv.Token(i64Min + 1), many: <wv.Token>[t]);
  expect(wv.sumHolder(mixed) == i64Min + 5, 'i64 extremes in tokens');
  final empty = wv.Holder(primary: wv.Token(7), many: <wv.Token>[]);
  expect(wv.sumHolder(empty) == 7, 'holder with empty list');

  // Release: dispose every wrapper, twice; disposed wrappers are refused at
  // encode time (a Dart StateError, never a native double free), and
  // wrappers that share an object with a disposed one stay valid.
  p.dispose();
  p.dispose();
  expect(h.primary.value() == 10, 'h.primary alive after primaryOf disposed');
  try {
    wv.sumHolder(viaPrimaryOf);
    throw StateError('expected StateError encoding a disposed token');
  } on StateError catch (e) {
    expect(e.message.contains('dispose'), 'disposed token in a buffer');
  }
  t.dispose();
  t.dispose();
  try {
    t.value();
    throw StateError('expected StateError after dispose');
  } on StateError catch (_) {}
  for (final holder in <wv.Holder>[h, bare]) {
    holder.primary.dispose();
    holder.spare?.dispose();
    for (final m in holder.many) {
      m.dispose();
      m.dispose();
    }
  }
  lookalike.dispose();
  mixed.primary.dispose();
  mixed.spare!.dispose();
  empty.primary.dispose();

  // Fresh objects still work after all of the above were released.
  final again = wv.makeHolder(100, true);
  expect(wv.sumHolder(again) == 100 + 101 + 102 + 103 + 104, 'fresh holder');
  again.primary.dispose();
  again.spare!.dispose();
  for (final m in again.many) {
    m.dispose();
  }
}

void main() {
  checkScalars();
  checkComposite();
  checkShapes();
  checkDirect();
  checkHolder();
  print('dart/codec: OK');
}
