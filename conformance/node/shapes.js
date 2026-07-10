// Conformance consumer: shapes sample, Node (N-API) target.
//
// Drives the generated rich (algebraic) enum surface through the wrapper
// layer (index.js): the Shape class with its per-variant static factories
// (`Shape.circle(radius)`), the `tag()` reader against the frozen `Shape.Tag`
// discriminant map, the namespaced per-variant getters (`circleRadius`), the
// explicit `destroy()`, and the free functions that take and return the rich
// enum as class instances (`describe`, `scale`) under the default
// lowerCamelCase, module-prefix-stripped names. Also covers the expanded
// numerics (f64 + f32 fields, u8 field, list<u8> in, u64 out). Mirrors
// conformance/c/shapes.c and conformance/cpp/shapes.cpp. Exits non-zero on
// any failed assertion; prints `node/shapes: OK` on success.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-shapes/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/shapes/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'shapes', 'node', 'index.js')
);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

function approx(actual, expected, eps, msg) {
  expect(Math.abs(actual - expected) < eps, `${msg} (got ${actual}, want ${expected})`);
}

const Shape = wv.Shape;

// Empty (unit variant): tag only, no payload.
const empty = Shape.empty();
expect(empty instanceof Shape, 'factory returns a Shape instance');
expect(empty.tag() === Shape.Tag.Empty, 'empty tag is Empty');

// Circle (f64 payload).
const circle = Shape.circle(2.5);
expect(circle.tag() === Shape.Tag.Circle, 'circle tag is Circle');
approx(circle.circleRadius, 2.5, 1e-9, 'circle radius');

// Rectangle (two f32 payloads).
const rect = Shape.rectangle(3.0, 4.0);
expect(rect.tag() === Shape.Tag.Rectangle, 'rectangle tag is Rectangle');
approx(rect.rectangleWidth, 3.0, 1e-6, 'rectangle width');
approx(rect.rectangleHeight, 4.0, 1e-6, 'rectangle height');

// Labeled (string + u8 payload).
const labeled = Shape.labeled('hex', 6);
expect(labeled.tag() === Shape.Tag.Labeled, 'labeled tag is Labeled');
expect(labeled.labeledLabel === 'hex', 'labeled label is "hex"');
expect(Number(labeled.labeledCount) === 6, 'labeled count is 6');

// describe: dispatch on the active variant (rich enum in, string out).
expect(wv.describe(circle) === 'circle(r=2.5)', 'describe(circle)');

// scale: rich enum in and out (returns a new owned instance).
const big = wv.scale(circle, 4.0);
expect(big instanceof Shape, 'scale returns a Shape instance');
expect(big.tag() === Shape.Tag.Circle, 'scaled tag is Circle');
approx(big.circleRadius, 10.0, 1e-9, 'scaled radius');

// Numerics: list<u8> in, u64 out.
expect(Number(wv.sumBytes([250, 250, 250, 250])) === 1000, 'sumBytes == 1000');

// Free every owned instance (destroy() is idempotent; the
// FinalizationRegistry backstops anything missed).
big.destroy();
labeled.destroy();
rect.destroy();
circle.destroy();
empty.destroy();

console.log('node/shapes: OK');
