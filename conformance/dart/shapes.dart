// Conformance consumer: shapes sample, Dart target.
//
// Drives the generated rich (algebraic) enum surface: the sealed `Shape`
// class, its per-variant subclasses (`ShapeCircle(...)`) with plain final
// fields, and the free functions that take and return `Shape` as value
// buffers. Also covers the expanded numerics (f32 fields, u8 field, u64
// return). Mirrors the assertions in conformance/c/shapes.c and
// conformance/cpp/shapes.cpp. Throws (non-zero exit) on any mismatch; prints
// `dart/shapes: OK` on success.
//
// Library selection follows the harness convention: the generated package name
// and library basename are substituted into the import sentinels, and the
// producer cdylib is chosen at runtime via the WEAVEFFI_LIBRARY env var read by
// the generated _openLibrary().

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

bool near(num a, num b) => (a - b).abs() < 1e-6;

void main() {
  // Empty (unit variant): no fields.
  final wv.Shape empty = wv.ShapeEmpty();
  expect(empty is wv.ShapeEmpty, 'empty variant');

  // Circle (f64 payload).
  final circle = wv.ShapeCircle(2.5);
  expect(near(circle.radius, 2.5), 'circle radius == 2.5');

  // Rectangle (two f32 payloads).
  final rect = wv.ShapeRectangle(3.0, 4.0);
  expect(near(rect.width, 3.0), 'rectangle width == 3.0');
  expect(near(rect.height, 4.0), 'rectangle height == 4.0');

  // Labeled (string + u8 payload).
  final labeled = wv.ShapeLabeled('hex', 6);
  expect(labeled.label == 'hex', 'labeled label == hex');
  expect(labeled.count == 6, 'labeled count == 6');

  // describe: dispatch on the active variant of the buffered parameter.
  expect(wv.describe(circle) == 'circle(r=2.5)', 'describe circle');

  // scale: rich enum in and out; the result decodes to the matching subclass.
  final big = wv.scale(circle, 4.0);
  switch (big) {
    case wv.ShapeCircle(:final radius):
      expect(near(radius, 10.0), 'scaled radius == 10.0');
    default:
      throw StateError('scaled shape is not a circle (got $big)');
  }

  // numerics: list<u8> in, u64 out.
  final total = wv.sumBytes(<int>[250, 250, 250, 250]);
  expect(total == 1000, 'sum_bytes == 1000 (got $total)');

  print('dart/shapes: OK');
}
