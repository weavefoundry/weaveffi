// Conformance consumer: shapes sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real producer
// compiled to wasm, exercising rich (algebraic) enums as value types: plain
// tagged objects ({ tag: 'Circle', radius: 2.5 }) serialized into value
// buffers at the boundary, with no wrapper class, factories, or free().
// Covers every variant shape (unit, f64, two f32s, string + u8) through
// describe (enum in, string out) and scale (enum in and out), the consumer-side
// rejection of an unknown variant tag, plus the expanded numerics ([u8] in,
// u64 out as BigInt). Mirrors conformance/c/shapes.c.
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled shapes.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)

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

// describe: rich enum in (a plain tagged object), string out; dispatches on
// the active variant.
const circle = { tag: 'Circle', radius: 2.5 };
expect(api.shapes.describe(circle) === 'circle(r=2.5)', 'describe(circle)');
expect(api.shapes.describe({ tag: 'Empty' }) === 'empty', 'describe(empty)');
expect(
  /rect/.test(api.shapes.describe({ tag: 'Rectangle', width: 3.0, height: 4.0 })),
  'describe(rectangle)'
);
expect(
  /hex/.test(api.shapes.describe({ tag: 'Labeled', label: 'hex', count: 6 })),
  'describe(labeled)'
);

// scale: rich enum in and out. The circle's f64 payload round-trips scaled.
const big = api.shapes.scale(circle, 4.0);
expect(big.tag === 'Circle', "scaled.tag === 'Circle'");
expect(approx(big.radius, 10.0, 1e-9), 'scaled.radius == 10.0');

// Rectangle: two f32 payloads survive the round trip (scale by 2).
const rect2 = api.shapes.scale({ tag: 'Rectangle', width: 3.0, height: 4.0 }, 2.0);
expect(rect2.tag === 'Rectangle', "rect2.tag === 'Rectangle'");
expect(approx(rect2.width, 6.0, 1e-6), 'rect2.width == 6.0');
expect(approx(rect2.height, 8.0, 1e-6), 'rect2.height == 8.0');

// Labeled: the string + u8 payload passes through scale unchanged.
const labeled2 = api.shapes.scale({ tag: 'Labeled', label: 'hex', count: 6 }, 9.0);
expect(labeled2.tag === 'Labeled', "labeled2.tag === 'Labeled'");
expect(labeled2.label === 'hex', "labeled2.label == 'hex'");
expect(labeled2.count === 6, 'labeled2.count == 6');

// Empty: a unit variant is tag-only in both directions.
const empty2 = api.shapes.scale({ tag: 'Empty' }, 3.0);
expect(empty2.tag === 'Empty', "empty2.tag === 'Empty'");

// An unknown variant tag is a consumer-side marshalling failure, rejected
// before anything crosses the boundary.
let tagErr = null;
try { api.shapes.describe({ tag: 'Pentagon' }); } catch (e) { tagErr = e; }
expect(tagErr instanceof mod.WeaveFFIError, 'unknown tag -> WeaveFFIError');
expect(tagErr && tagErr.code === -3, `unknown tag -> marshalling code -3 (got ${tagErr && tagErr.code})`);
expect(tagErr && tagErr.message.includes('Pentagon'), 'unknown tag error names the tag');
expect(api.shapes.describe({ tag: 'Empty' }) === 'empty', 'module usable after the rejected tag');

// Floating-point edge values inside the rich enum payloads survive the round
// trip: NaN, infinities, and -0 in the f64 and f32 slots.
const nanCircle = api.shapes.scale({ tag: 'Circle', radius: NaN }, 1.0);
expect(Number.isNaN(nanCircle.radius), 'NaN radius survives');
const negZero = api.shapes.scale({ tag: 'Circle', radius: -0 }, 1.0);
expect(Object.is(negZero.radius, -0), '-0 radius keeps its sign');
const inf = api.shapes.scale({ tag: 'Rectangle', width: Infinity, height: -Infinity }, 1.0);
expect(inf.width === Infinity && inf.height === -Infinity, 'infinities survive in f32 slots');

// numerics: [u8] in (canonicalized to bytes), u64 out (BigInt);
// lowerCamelCase wrapper name.
expect(api.shapes.sumBytes([250, 250, 250, 250]) === 1000n, 'sumBytes == 1000n');
expect(typeof api.shapes.sumBytes([1]) === 'bigint', 'sumBytes returns a BigInt');
expect(api.shapes.sumBytes(new Uint8Array([255, 255])) === 510n, 'sumBytes accepts a Uint8Array');
expect(api.shapes.sumBytes([]) === 0n, 'sumBytes of nothing is 0n');

if (failures === 0) {
  console.log('wasm/shapes: OK');
} else {
  console.error(`wasm/shapes: ${failures} failure(s)`);
  process.exit(1);
}
