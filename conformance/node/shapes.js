// Conformance consumer: shapes sample, Node (N-API) target.
//
// Drives the generated rich (algebraic) enum surface exposed by the native
// addon: the opaque-handle constructors (`shapes_Shape_<variant>_new`), the
// int32 tag reader (`shapes_Shape_tag`), the variant-namespaced field getters
// (`shapes_Shape_<variant>_get_<field>`), and the destructor
// (`shapes_Shape_destroy`), plus the free functions that take and return the
// rich enum as the same opaque handle (`shapes_describe`, `shapes_scale`). Also
// covers the expanded numerics (f64 + f32 fields, u8 field, list<u8> in, u64
// out). Mirrors conformance/c/shapes.c and conformance/cpp/shapes.cpp. Loads the
// built addon via WV_ADDON, exactly like kvstore.js. Exits non-zero on any
// failed assertion; prints `node/shapes: OK` on success.

'use strict';

const addon = require(process.env.WV_ADDON);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

function approx(actual, expected, eps, msg) {
  expect(Math.abs(actual - expected) < eps, `${msg} (got ${actual}, want ${expected})`);
}

// Discriminant values, matching the generated Shape.Tag map / C constants.
const Tag = { Empty: 0, Circle: 1, Rectangle: 2, Labeled: 3 };

// Empty (unit variant): tag only, no payload.
const empty = addon.shapes_Shape_empty_new();
expect(addon.shapes_Shape_tag(empty) === Tag.Empty, 'empty tag is Empty');

// Circle (f64 payload).
const circle = addon.shapes_Shape_circle_new(2.5);
expect(addon.shapes_Shape_tag(circle) === Tag.Circle, 'circle tag is Circle');
approx(addon.shapes_Shape_circle_get_radius(circle), 2.5, 1e-9, 'circle radius');

// Rectangle (two f32 payloads).
const rect = addon.shapes_Shape_rectangle_new(3.0, 4.0);
expect(addon.shapes_Shape_tag(rect) === Tag.Rectangle, 'rectangle tag is Rectangle');
approx(addon.shapes_Shape_rectangle_get_width(rect), 3.0, 1e-6, 'rectangle width');
approx(addon.shapes_Shape_rectangle_get_height(rect), 4.0, 1e-6, 'rectangle height');

// Labeled (string + u8 payload).
const labeled = addon.shapes_Shape_labeled_new('hex', 6);
expect(addon.shapes_Shape_tag(labeled) === Tag.Labeled, 'labeled tag is Labeled');
expect(addon.shapes_Shape_labeled_get_label(labeled) === 'hex', 'labeled label is "hex"');
expect(Number(addon.shapes_Shape_labeled_get_count(labeled)) === 6, 'labeled count is 6');

// describe: dispatch on the active variant (rich enum in, string out).
expect(addon.shapes_describe(circle) === 'circle(r=2.5)', 'describe(circle)');

// scale: rich enum in and out (returns a new owned handle).
const big = addon.shapes_scale(circle, 4.0);
expect(addon.shapes_Shape_tag(big) === Tag.Circle, 'scaled tag is Circle');
approx(addon.shapes_Shape_circle_get_radius(big), 10.0, 1e-9, 'scaled radius');

// Numerics: list<u8> in, u64 out.
expect(Number(addon.shapes_sum_bytes([250, 250, 250, 250])) === 1000, 'sum_bytes == 1000');

// Free every owned handle (the C ABI hands back owned objects).
addon.shapes_Shape_destroy(big);
addon.shapes_Shape_destroy(labeled);
addon.shapes_Shape_destroy(rect);
addon.shapes_Shape_destroy(circle);
addon.shapes_Shape_destroy(empty);

console.log('node/shapes: OK');
