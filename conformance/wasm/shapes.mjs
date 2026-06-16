// Conformance consumer: shapes sample, WASM (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real producer
// compiled to wasm, exercising rich (algebraic) enums end to end: the opaque
// `Shape` wrapper class, its `tag` discriminant reader + `Tag` map, every
// per-variant static factory (Empty / Circle / Rectangle / Labeled), the
// namespaced per-variant field getters (camelCase, e.g. circleRadius), and the
// `free()` cleanup. Also covers the free functions that take and return a rich
// enum (describe / scale) plus the expanded numerics (f32 fields, u8 field,
// list<u8> in, u64 out as BigInt). Mirrors conformance/c/shapes.c and
// conformance/cpp/shapes.cpp.
//
// Inputs come from the harness:
//   WV_WASM — path to the compiled shapes.wasm
//   WV_JS   — path to the generated weaveffi_wasm.js (ESM)

import fs from 'fs';

const WASM = process.env.WV_WASM;
const JS = process.env.WV_JS;
if (!WASM || !JS) {
  console.error('WV_WASM and WV_JS must be set');
  process.exit(2);
}

// Node has no file:// fetch; shim it so the generated loader can read the .wasm.
globalThis.fetch = async (url) => ({ arrayBuffer: async () => fs.readFileSync(url) });

const mod = await import(JS);
const api = await mod.loadWeaveffiWasm(WASM);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}
function approx(a, b, eps) {
  return typeof a === 'number' && Math.abs(a - b) < eps;
}

// Plain C-style enum still crosses by value as a frozen discriminant object.
expect(mod.Channel && mod.Channel.Green === 1, 'plain enum Channel exported');

const Shape = api.shapes.Shape;
const Tag = Shape.Tag;
expect(Tag && Tag.Empty === 0 && Tag.Labeled === 3, 'Shape.Tag discriminant map');

// Empty (unit variant): tag only, no payload.
const empty = Shape.empty();
expect(empty && empty._handle > 0, 'Shape.empty -> handle');
expect(empty.tag === Tag.Empty, 'empty.tag === Empty');

// Circle (f64 payload).
const circle = Shape.circle(2.5);
expect(circle.tag === Tag.Circle, 'circle.tag === Circle');
expect(approx(circle.circleRadius, 2.5, 1e-9), 'circle.circleRadius == 2.5');

// Rectangle (two f32 payloads).
const rect = Shape.rectangle(3.0, 4.0);
expect(rect.tag === Tag.Rectangle, 'rect.tag === Rectangle');
expect(approx(rect.rectangleWidth, 3.0, 1e-6), 'rect.rectangleWidth == 3.0');
expect(approx(rect.rectangleHeight, 4.0, 1e-6), 'rect.rectangleHeight == 4.0');

// Labeled (string + u8 payload).
const labeled = Shape.labeled('hex', 6);
expect(labeled.tag === Tag.Labeled, 'labeled.tag === Labeled');
expect(labeled.labeledLabel === 'hex', 'labeled.labeledLabel == "hex"');
expect(labeled.labeledCount === 6, 'labeled.labeledCount == 6');

// describe: rich enum in, string out — dispatches on the active variant.
expect(api.shapes.describe(circle) === 'circle(r=2.5)', 'describe(circle)');

// scale: rich enum in and out.
const big = api.shapes.scale(circle, 4.0);
expect(big.tag === Tag.Circle, 'scaled.tag === Circle');
expect(approx(big.circleRadius, 10.0, 1e-9), 'scaled.circleRadius == 10.0');

// numerics: list<u8> in, u64 out (BigInt).
expect(api.shapes.sum_bytes([250, 250, 250, 250]) === 1000n, 'sum_bytes == 1000n');

// Cleanup: release every producer-owned handle exactly once.
big.free();
labeled.free();
rect.free();
circle.free();
empty.free();

if (failures === 0) {
  console.log('wasm/shapes: OK');
} else {
  console.error(`wasm/shapes: ${failures} failure(s)`);
  process.exit(1);
}
