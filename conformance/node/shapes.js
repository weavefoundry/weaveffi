// Conformance consumer: shapes sample, Node (N-API) target.
//
// Drives the generated rich (algebraic) enum surface through the wrapper
// layer (index.js): shapes are plain tagged objects ({ tag: 'Circle',
// radius: 2.5 }) that the wrapper packs into value buffers on the way in
// and unpacks on the way out. Covers every variant round-tripping through
// `scale` (unit, f64, f32 pair, and string + u8 payloads), `describe`
// dispatching on the active variant, and the expanded numerics (bytes in,
// u64 out) under the default lowerCamelCase, module-prefix-stripped names.
// Mirrors conformance/c/shapes.c and conformance/cpp/shapes.cpp. Exits
// non-zero on any failed assertion; prints `node/shapes: OK` on success.

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

// Empty (unit variant): tag only, no payload. Round-tripping through scale
// exercises pack and unpack of the bare tag.
const empty = { tag: 'Empty' };
expect(wv.describe(empty) === 'empty', 'describe(empty)');
const emptyBack = wv.scale(empty, 2.0);
expect(typeof emptyBack === 'object' && emptyBack !== null, 'scale returns an object');
expect(emptyBack.tag === 'Empty', 'empty tag round-trips');

// Circle (f64 payload).
const circle = { tag: 'Circle', radius: 2.5 };
expect(wv.describe(circle) === 'circle(r=2.5)', 'describe(circle)');
const big = wv.scale(circle, 4.0);
expect(big.tag === 'Circle', 'scaled tag is Circle');
approx(big.radius, 10.0, 1e-9, 'scaled radius');

// Rectangle (two f32 payloads).
const rect = { tag: 'Rectangle', width: 3.0, height: 4.0 };
expect(wv.describe(rect) === 'rectangle(3x4)', 'describe(rectangle)');
const grown = wv.scale(rect, 2.0);
expect(grown.tag === 'Rectangle', 'scaled tag is Rectangle');
approx(grown.width, 6.0, 1e-6, 'scaled rectangle width');
approx(grown.height, 8.0, 1e-6, 'scaled rectangle height');

// Labeled (string + u8 payload): scale leaves the payload alone, so the
// round trip checks both fields survive pack and unpack intact.
const labeled = { tag: 'Labeled', label: 'hex', count: 6 };
expect(wv.describe(labeled) === 'labeled(hex x6)', 'describe(labeled)');
const labeledBack = wv.scale(labeled, 3.0);
expect(labeledBack.tag === 'Labeled', 'labeled tag round-trips');
expect(labeledBack.label === 'hex', 'labeled label is "hex"');
expect(Number(labeledBack.count) === 6, 'labeled count is 6');

// Numerics: bytes in ([u8] canonicalizes to bytes, so a Buffer), u64 out.
expect(Number(wv.sumBytes(Buffer.from([250, 250, 250, 250]))) === 1000, 'sumBytes == 1000');

console.log('node/shapes: OK');
