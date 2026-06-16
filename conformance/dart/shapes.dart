// Conformance consumer: shapes sample, Dart target.
//
// Drives the generated rich (algebraic) enum wrapper: the opaque-object `Shape`
// class, its `ShapeTag` discriminant + `tag` reader, the per-variant factory
// constructors (`Shape.circle(...)`) and namespaced field getters
// (`circleRadius`), plus the free functions that take/return `Shape` by handle.
// Also covers the expanded numerics (f32 fields, u8 field, u64 return). Mirrors
// the assertions in conformance/c/shapes.c and conformance/cpp/shapes.cpp.
// Throws (non-zero exit) on any mismatch; prints `dart/shapes: OK` on success.
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
  // Empty (unit variant): tag only.
  final empty = wv.Shape.empty();
  expect(empty.tag == wv.ShapeTag.empty, 'empty tag');

  // Circle (f64 payload).
  final circle = wv.Shape.circle(2.5);
  expect(circle.tag == wv.ShapeTag.circle, 'circle tag');
  expect(near(circle.circleRadius, 2.5), 'circle radius == 2.5');

  // Rectangle (two f32 payloads).
  final rect = wv.Shape.rectangle(3.0, 4.0);
  expect(rect.tag == wv.ShapeTag.rectangle, 'rectangle tag');
  expect(near(rect.rectangleWidth, 3.0), 'rectangle width == 3.0');
  expect(near(rect.rectangleHeight, 4.0), 'rectangle height == 4.0');

  // Labeled (string + u8 payload).
  final labeled = wv.Shape.labeled('hex', 6);
  expect(labeled.tag == wv.ShapeTag.labeled, 'labeled tag');
  expect(labeled.labeledLabel == 'hex', 'labeled label == hex');
  expect(labeled.labeledCount == 6, 'labeled count == 6');

  // describe: dispatch on the active variant.
  expect(wv.describe(circle) == 'circle(r=2.5)', 'describe circle');

  // scale: rich enum in and out.
  final big = wv.scale(circle, 4.0);
  expect(big.tag == wv.ShapeTag.circle, 'scaled tag still circle');
  expect(near(big.circleRadius, 10.0), 'scaled radius == 10.0');

  // numerics: list<u8> in, u64 out.
  final total = wv.sumBytes(<int>[250, 250, 250, 250]);
  expect(total == 1000, 'sum_bytes == 1000 (got $total)');

  big.dispose();
  labeled.dispose();
  rect.dispose();
  circle.dispose();
  empty.dispose();

  print('dart/shapes: OK');
}
