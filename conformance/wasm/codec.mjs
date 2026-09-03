// Conformance consumer: codec sample, Wasm (wasm32-unknown-unknown) target.
//
// Checks the generated value-buffer codec (weaveffi_wasm.js) against the
// producer's round-trip oracle in both directions: `sample*` fixtures decoded
// field by field against concrete expected values (producer encodes, consumer
// decodes), `verify*` accepting the same fixture back (consumer encodes,
// producer decodes), `roundtrip*` returning consumer-built values with edge
// cases (empty strings, lists, and maps; unicode; i64/u64 extremes as BigInt,
// both by value across the wasm boundary and inside buffers; NaN, +/-Infinity,
// and -0 doubles; every Shape variant), the typed `Mismatch` error, and
// objects inside buffers through `Holder` (a Token field, an optional Token,
// a list of Tokens; `primaryOf` returning a wrapper to the same object;
// `samePrimary`; every wrapper released with an idempotent `close()`).
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled codec.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)
// Run with: node --experimental-wasm-type-reflection (for WebAssembly.Function).

import fs from 'fs';
import assert from 'node:assert';

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
const wv = api.codec;
const { WeaveFFIError, CodecError, Mismatch, Color } = mod;
const { Token } = wv;

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}
// Structural equality with SameValue semantics for primitives: BigInt by
// value, NaN equal to NaN, -0 distinct from +0, Uint8Array by contents.
function same(actual, expected, msg) {
  try {
    assert.deepStrictEqual(actual, expected);
  } catch (e) {
    console.error('assertion failed: ' + msg);
    console.error(e.message);
    failures++;
  }
}
function throwsMismatch(fn, msg) {
  try {
    fn();
    expect(false, msg + ': expected Mismatch');
  } catch (e) {
    expect(e instanceof Mismatch, msg + ': Mismatch instance (got ' + (e && e.constructor.name) + ')');
    expect(e instanceof CodecError, msg + ': extends CodecError');
    expect(e instanceof WeaveFFIError, msg + ': extends WeaveFFIError');
    expect(e.code === 1 && Mismatch.CODE === 1 && CodecError.Mismatch === Mismatch, msg + ': code 1');
  }
}
const u8 = (...bytes) => new Uint8Array(bytes);

// The C-style enum is a frozen object of declared values.
expect(Color.Red === 0 && Color.Green === 1 && Color.Blue === 7, 'Color values');
expect(Object.isFrozen(Color), 'Color is frozen');
expect(typeof Token === 'function', 'Token class exposed on api.codec');

// --- Scalars ----------------------------------------------------------------
const canonicalScalars = {
  i8_value: -8,
  u8_value: 200,
  i16_value: -16000,
  u16_value: 60000,
  i32_value: -2000000000,
  u32_value: 4000000000,
  i64_value: -9007199254740993n,
  u64_value: 18446744073709551615n,
  f32_value: 1.5,
  f64_value: -2.25e100,
  flag: true,
  color: Color.Blue,
};
const scalars = wv.sampleScalars();
same(scalars, canonicalScalars, 'sampleScalars matches the canonical fixture');
expect(typeof scalars.i64_value === 'bigint', 'i64 field is a BigInt');
expect(typeof scalars.u64_value === 'bigint', 'u64 field is a BigInt');
expect(typeof scalars.i32_value === 'number' && typeof scalars.u32_value === 'number', '32-bit fields are numbers');
expect(typeof scalars.flag === 'boolean', 'bool field is a boolean');
expect(scalars.i64_value === -9007199254740993n, 'i64 beyond 2^53 is exact');
expect(scalars.u64_value === 18446744073709551615n, 'u64::MAX is exact');
expect(wv.verifyScalars(scalars) === true, 'verifyScalars accepts the decoded fixture');
expect(wv.verifyScalars(canonicalScalars) === true, 'verifyScalars accepts a hand-built fixture');
same(wv.roundtripScalars(scalars), canonicalScalars, 'roundtripScalars preserves every field');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, u64_value: 18446744073709551614n }), 'u64 off by one');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, i64_value: -9007199254740992n }), 'i64 off by one');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, flag: false }), 'flag flipped');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, color: Color.Red }), 'color changed');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, f32_value: 1.5000001 }), 'f32 changed');

// Consumer-built extremes, including doubles JS can express but JSON can't.
const extremes = {
  i8_value: -128,
  u8_value: 255,
  i16_value: -32768,
  u16_value: 65535,
  i32_value: -2147483648,
  u32_value: 4294967295,
  i64_value: -9223372036854775808n,
  u64_value: 0n,
  f32_value: -0,
  f64_value: NaN,
  flag: false,
  color: Color.Red,
};
const extremesBack = wv.roundtripScalars(extremes);
same(extremesBack, extremes, 'extreme scalars round-trip');
expect(Object.is(extremesBack.f32_value, -0), 'f32 -0 keeps its sign');
expect(Number.isNaN(extremesBack.f64_value), 'f64 NaN survives');
const maxes = {
  ...extremes,
  i8_value: 127,
  i16_value: 32767,
  i32_value: 2147483647,
  i64_value: 9223372036854775807n,
  u64_value: 18446744073709551615n,
  f32_value: Infinity,
  f64_value: -Infinity,
  color: Color.Green,
};
same(wv.roundtripScalars(maxes), maxes, 'max scalars round-trip');
// A number for a 64-bit field is accepted by the encoder (BigInt(v)).
same(
  wv.roundtripScalars({ ...extremes, i64_value: 42, u64_value: 7 }),
  { ...extremes, i64_value: 42n, u64_value: 7n },
  'number literals widen to BigInt on the way back'
);
// A non-integral number can't become a BigInt: the encoder throws before
// anything crosses the boundary.
try {
  wv.roundtripScalars({ ...extremes, i64_value: 1.5 });
  expect(false, 'fractional i64 should throw');
} catch (e) {
  expect(e instanceof RangeError, `fractional i64 is a RangeError (got ${e && e.constructor.name})`);
}

// Direct-family 64-bit params and returns are BigInt end to end: wasm i64
// maps to BigInt in both directions with no precision loss.
for (const v of [0n, 1n, -1n, 9007199254740993n, -9007199254740993n, 9223372036854775807n, -9223372036854775808n]) {
  const back = wv.roundtripI64(v);
  expect(typeof back === 'bigint' && back === v, `roundtripI64(${v}) (got ${back})`);
}
expect(wv.roundtripI64(5) === 5n, 'roundtripI64 accepts a number and returns a BigInt');
expect(wv.roundtripI64(-5) === -5n, 'roundtripI64 accepts a negative number');
for (const v of [0n, 1n, 9223372036854775808n, 18446744073709551615n]) {
  const back = wv.roundtripU64(v);
  expect(typeof back === 'bigint' && back === v, `roundtripU64(${v}) (got ${back})`);
}
// Out-of-range BigInts follow the JS-to-wasm i64 coercion (ToBigInt64,
// modulo 2^64) rather than throwing; pin that down so a change is visible.
expect(wv.roundtripI64(9223372036854775808n) === -9223372036854775808n, 'i64 wraps modulo 2^64 at the wasm boundary');
expect(wv.roundtripU64(-1n) === 18446744073709551615n, 'u64 wraps modulo 2^64 at the wasm boundary');
try {
  wv.roundtripI64(1.5);
  expect(false, 'fractional i64 param should throw');
} catch (e) {
  expect(e instanceof RangeError, `fractional i64 param is a RangeError (got ${e && e.constructor.name})`);
}
try {
  wv.roundtripI64('nope');
  expect(false, 'non-numeric i64 param should throw');
} catch (e) {
  expect(e instanceof SyntaxError, `non-numeric i64 param is a SyntaxError (got ${e && e.constructor.name})`);
}

// Doubles by value: wasm f64 is a JS number, so every bit pattern survives.
for (const v of [0, -0, 1.5, -2.25e100, 5e-324, Number.MAX_VALUE, Number.MIN_VALUE, Number.EPSILON, Infinity, -Infinity]) {
  expect(Object.is(wv.roundtripF64(v), v), `roundtripF64(${v})`);
}
expect(Number.isNaN(wv.roundtripF64(NaN)), 'roundtripF64(NaN)');
expect(wv.roundtripBool(true) === true && wv.roundtripBool(false) === false, 'roundtripBool');
expect(typeof wv.roundtripBool(true) === 'boolean', 'roundtripBool returns a boolean, not an i32');
expect(wv.roundtripColor(Color.Blue) === 7, 'roundtripColor(Blue) == 7');
expect(wv.roundtripColor(Color.Red) === 0, 'roundtripColor(Red) == 0');
expect(wv.roundtripColor(Color.Green) === Color.Green, 'roundtripColor(Green)');

// Strings and bytes at the top level: NUL-terminated UTF-8 in, owned
// C string out (freed by the glue), and byte buffers as Uint8Array.
for (const s of ['', 'ascii', 'héllo wörld ✓', '日本語', 'emoji 🎉 pair', 'a'.repeat(70000), '\u{10FFFF}']) {
  expect(wv.roundtripString(s) === s, `roundtripString(${JSON.stringify(s.slice(0, 20))}...)`);
}
const allBytes = new Uint8Array(Array.from({ length: 256 }, (_, i) => i));
const allBack = wv.roundtripBytes(allBytes);
expect(allBack instanceof Uint8Array, 'roundtripBytes returns a Uint8Array');
same(allBack, allBytes, 'roundtripBytes(0..255)');
const emptyBytes = wv.roundtripBytes(new Uint8Array(0));
expect(emptyBytes instanceof Uint8Array && emptyBytes.length === 0, 'roundtripBytes(empty) is an empty Uint8Array');
same(wv.roundtripBytes([1, 2, 3]), u8(1, 2, 3), 'roundtripBytes accepts a plain array');
same(wv.roundtripBytes(Buffer.from([7, 8])), u8(7, 8), 'roundtripBytes accepts a Buffer');
const big = new Uint8Array(200000);
for (let i = 0; i < big.length; i++) big[i] = (i * 31) & 0xff;
same(wv.roundtripBytes(big), big, 'roundtripBytes(200000 bytes) (memory growth)');
// The returned copy is detached from linear memory: mutating it doesn't
// affect later calls, and it survives memory growth.
const copy = wv.roundtripBytes(u8(1, 2, 3));
copy[0] = 99;
same(wv.roundtripBytes(u8(1, 2, 3)), u8(1, 2, 3), 'returned bytes are an owned copy');

// Optionals and maps at the top level.
expect(wv.roundtripOptI64(null) === null, 'roundtripOptI64(null)');
expect(wv.roundtripOptI64(undefined) === null, 'roundtripOptI64(undefined) is null');
expect(wv.roundtripOptI64(-9223372036854775808n) === -9223372036854775808n, 'roundtripOptI64(i64::MIN)');
expect(wv.roundtripOptI64(9223372036854775807n) === 9223372036854775807n, 'roundtripOptI64(i64::MAX)');
expect(wv.roundtripOptI64(0n) === 0n, 'roundtripOptI64(0n) is present');
expect(wv.roundtripOptI64(0) === 0n, 'roundtripOptI64(0) is present (a number widens)');
same(wv.roundtripMap({}), {}, 'roundtripMap({})');
same(
  wv.roundtripMap({ a: 1n, '': -2n, 'ключ': 9223372036854775807n, z: -9223372036854775808n }),
  { a: 1n, '': -2n, 'ключ': 9223372036854775807n, z: -9223372036854775808n },
  'roundtripMap with odd keys and extremes'
);
same(wv.roundtripMap({ n: 3 }), { n: 3n }, 'roundtripMap widens number values to BigInt');

// --- Shape (rich enum) ------------------------------------------------------
const shapeCases = [
  { tag: 'Empty' },
  { tag: 'Circle', radius: 2.5 },
  { tag: 'Circle', radius: -0 },
  { tag: 'Circle', radius: NaN },
  { tag: 'Circle', radius: -Infinity },
  { tag: 'Rect', width: 1.0, height: 0.5 },
  { tag: 'Rect', width: -0, height: Infinity },
  { tag: 'Labeled', label: 'tag', count: 3 },
  { tag: 'Labeled', label: '', count: -2147483648 },
  { tag: 'Labeled', label: 'ünïcödé ✓', count: 2147483647 },
  { tag: 'Nested', inner: canonicalScalars, note: 'n' },
  { tag: 'Nested', inner: extremes, note: null },
  { tag: 'Nested', inner: maxes, note: '' },
];
for (const s of shapeCases) {
  same(wv.roundtripShape(s), s, `roundtripShape(${JSON.stringify(s, (_, v) => (typeof v === 'bigint' ? v.toString() : v)).slice(0, 60)})`);
}
same(wv.roundtripShapes(shapeCases), shapeCases, 'roundtripShapes(all variants)');
same(wv.roundtripShapes([]), [], 'roundtripShapes([])');
expect(wv.describeShape({ tag: 'Empty' }) === 'Empty', 'describeShape(Empty)');
expect(wv.describeShape({ tag: 'Circle', radius: 2.5 }) === 'Circle { radius: 2.5 }', `describeShape(Circle) (got ${wv.describeShape({ tag: 'Circle', radius: 2.5 })})`);
expect(wv.describeShape({ tag: 'Labeled', label: 'tag', count: 3 }) === 'Labeled { label: "tag", count: 3 }', 'describeShape(Labeled)');
const nestedBack = wv.roundtripShape({ tag: 'Nested', inner: extremes, note: null });
expect(Number.isNaN(nestedBack.inner.f64_value) && nestedBack.note === null, 'nested NaN and absent note');
expect(Object.is(nestedBack.inner.f32_value, -0), 'nested f32 -0 keeps its sign');
// An unknown tag is rejected by the encoder before anything is sent.
try {
  wv.roundtripShape({ tag: 'Hexagon' });
  expect(false, 'unknown tag should throw');
} catch (e) {
  expect(e instanceof WeaveFFIError && e.code === -3, `unknown tag is a marshalling error (got ${e && e.code})`);
  expect(e.message.includes('Hexagon'), 'unknown tag error names the tag');
}
// The module keeps working after a consumer-side encode failure.
same(wv.roundtripShape({ tag: 'Empty' }), { tag: 'Empty' }, 'roundtripShape works after an encode failure');

// --- Composite --------------------------------------------------------------
const canonicalComposite = {
  name: 'héllo wörld ✓',
  blob: u8(0, 1, 2, 253, 254, 255),
  some_i64: -9223372036854775808n,
  none_i64: null,
  some_text: '',
  names: ['a', '', 'ccc'],
  matrix: [[1, 2, 3], [], [-4]],
  empty: [],
  by_name: { one: 1n, two: 2n, neg: -3n },
  by_id: { '-1': canonicalScalars, '42': { ...canonicalScalars, flag: false } },
  scalars: canonicalScalars,
  shape: { tag: 'Labeled', label: 'tag', count: 3 },
  shapes: [
    { tag: 'Empty' },
    { tag: 'Circle', radius: 2.5 },
    { tag: 'Rect', width: 1.0, height: 0.5 },
    { tag: 'Labeled', label: '', count: -1 },
    { tag: 'Nested', inner: canonicalScalars, note: 'n' },
  ],
  maybe_shape: { tag: 'Nested', inner: canonicalScalars, note: null },
  maybe_list: u8(9, 8),
  sparse: [true, null, false],
  colors: [Color.Red, Color.Green, Color.Blue],
};
const composite = wv.sampleComposite();
same(composite, canonicalComposite, 'sampleComposite matches the canonical fixture');
expect(composite.blob instanceof Uint8Array, 'blob is a Uint8Array');
expect(composite.maybe_list instanceof Uint8Array, 'maybe_list is a Uint8Array');
expect(typeof composite.some_i64 === 'bigint', 'some_i64 is a BigInt');
expect(composite.none_i64 === null, 'absent optional is null');
expect(composite.some_text === '', 'present empty optional string is ""');
expect(typeof composite.by_name.one === 'bigint', 'map values are BigInt');
expect(composite.by_id[42].flag === false && composite.by_id[-1].flag === true, 'i32-keyed map indexes by number');
expect(composite.sparse[1] === null && composite.sparse[0] === true, 'list of optionals');
expect(wv.verifyComposite(composite) === true, 'verifyComposite accepts the decoded fixture');
expect(wv.verifyComposite(canonicalComposite) === true, 'verifyComposite accepts a hand-built fixture');
same(wv.roundtripComposite(composite), canonicalComposite, 'roundtripComposite preserves every field');
const described = wv.describeComposite(composite);
expect(
  typeof described === 'string' && described.startsWith('Composite {') && described.includes('name: "héllo wörld ✓"'),
  `describeComposite renders (got ${described.slice(0, 40)}...)`
);
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, sparse: [true, true, false] }), 'sparse changed');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, none_i64: 0n }), 'absent optional made present');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, by_name: { one: 1n, two: 2n } }), 'map entry missing');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, name: 'hello world' }), 'unicode name changed');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, blob: u8(0, 1, 2, 253, 254) }), 'blob shortened');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, maybe_shape: null }), 'optional shape removed');
// The module keeps working after a typed error.
expect(wv.verifyComposite(composite) === true, 'verifyComposite works after Mismatch errors');

// A consumer-built composite with edge values in every position.
const edge = {
  name: '',
  blob: new Uint8Array(0),
  some_i64: 9223372036854775807n,
  none_i64: -1n,
  some_text: null,
  names: [],
  matrix: [[], [-2147483648, 2147483647]],
  empty: [NaN, -0, Infinity, -Infinity, 5e-324],
  by_name: {},
  by_id: { '-2147483648': extremes, '0': maxes, '2147483647': canonicalScalars },
  scalars: extremes,
  shape: { tag: 'Empty' },
  shapes: [],
  maybe_shape: null,
  maybe_list: null,
  sparse: [null, null],
  colors: [],
};
const edgeBack = wv.roundtripComposite(edge);
same(edgeBack, edge, 'edge composite round-trips');
expect(Number.isNaN(edgeBack.empty[0]) && Object.is(edgeBack.empty[1], -0), 'NaN and -0 inside a list');
expect(edgeBack.by_id[-2147483648].i64_value === -9223372036854775808n, 'i32::MIN map key with i64::MIN value');
expect(edgeBack.by_id[0].u64_value === 18446744073709551615n, 'u64::MAX inside a map value');
const bigList = {
  ...edge,
  names: Array.from({ length: 1000 }, (_, i) => 'name-' + i),
  blob: allBytes,
  maybe_list: u8(255),
  sparse: Array.from({ length: 333 }, (_, i) => (i % 3 === 0 ? null : i % 3 === 1)),
  by_name: Object.fromEntries(Array.from({ length: 500 }, (_, i) => ['k' + i, BigInt(i) * 1000000007n])),
};
same(wv.roundtripComposite(bigList), bigList, 'large composite round-trips (writer growth)');

// --- Holder: objects inside buffers -----------------------------------------
const holder = wv.makeHolder(10n, true);
expect(holder.primary instanceof Token, 'holder.primary is a Token');
expect(holder.primary._handle > 0, 'holder.primary wraps a live handle');
expect(holder.primary.value() === 10n, `primary value 10n (got ${holder.primary.value()})`);
expect(holder.spare instanceof Token && holder.spare.value() === 11n, 'spare is Token(11)');
expect(Array.isArray(holder.many) && holder.many.length === 3, 'many has 3 tokens');
same(holder.many.map((t) => t.value()), [12n, 13n, 14n], 'many values 12..14');
expect(new Set([holder.primary, holder.spare, ...holder.many].map((t) => t._handle)).size === 5, 'five distinct objects');
expect(wv.sumHolder(holder) === 60n, `sumHolder == 60n (got ${wv.sumHolder(holder)})`);
// Encoding clones each handle, so the wrappers are still usable afterwards.
expect(holder.primary.value() === 10n && holder.many[2].value() === 14n, 'tokens usable after sumHolder');
expect(wv.sumHolder(holder) === 60n, 'sumHolder again (fresh clones each call)');

// primaryOf returns a new wrapper over the same underlying object.
const primary = wv.primaryOf(holder);
expect(primary instanceof Token && primary !== holder.primary, 'primaryOf returns a distinct wrapper');
expect(primary._handle === holder.primary._handle, 'primaryOf points at the same object');
expect(primary.value() === 10n, 'primaryOf value 10n');
expect(wv.samePrimary(holder, { primary, spare: null, many: [] }) === true, 'primaryOf aliases holder.primary');
expect(wv.samePrimary(holder, holder) === true, 'samePrimary(holder, holder)');
const other = wv.makeHolder(10n, true);
expect(wv.samePrimary(holder, other) === false, 'distinct holders with equal values are different objects');
expect(wv.sumHolder(other) === 60n, 'other sums to 60n as well');

const noSpare = wv.makeHolder(-5n, false);
expect(noSpare.spare === null, 'spare absent is null');
expect(noSpare.primary.value() === -5n, 'negative base');
expect(wv.sumHolder(noSpare) === -5n + -3n + -2n + -1n, 'sumHolder without spare');
const wrapped = wv.makeHolder(9223372036854775807n, true);
expect(wrapped.primary.value() === 9223372036854775807n, 'i64::MAX base');
expect(wrapped.spare.value() === -9223372036854775808n, 'spare wraps to i64::MIN');

// Consumer-constructed tokens in every buffered position.
const t1 = new Token(100n);
const t2 = new Token(200);
const t3 = new Token(-9223372036854775808n);
expect(t1.value() === 100n && t2.value() === 200n && t3.value() === -9223372036854775808n, 'constructed token values');
const mine = { primary: t1, spare: t2, many: [t3, t1, t1] };
expect(wv.sumHolder(mine) === 100n + 200n + -9223372036854775808n + 100n + 100n, 'sumHolder over consumer tokens');
expect(wv.sumHolder({ primary: t1, spare: null, many: [] }) === 100n, 'sumHolder with no spare and empty many');
expect(wv.sumHolder({ primary: t1, spare: undefined, many: undefined }) === 100n, 'undefined spare and many are absent/empty');
expect(wv.samePrimary(mine, { primary: t1, spare: null, many: [] }) === true, 'same consumer token in two holders');
expect(wv.samePrimary(mine, { primary: t2, spare: null, many: [] }) === false, 'different consumer tokens');
const primaryMine = wv.primaryOf(mine);
expect(primaryMine._handle === t1._handle && primaryMine.value() === 100n, 'primaryOf a consumer holder');
expect(t1.value() === 100n, 't1 alive after being sent three times and returned once');

// Release everything: close is idempotent, and a closed token can't be
// encoded (borrow after close is -3) or used.
const everyToken = [
  holder.primary, holder.spare, ...holder.many, primary,
  other.primary, other.spare, ...other.many,
  noSpare.primary, ...noSpare.many,
  wrapped.primary, wrapped.spare, ...wrapped.many,
  t1, t2, t3, primaryMine,
];
for (const t of everyToken) t.close();
for (const t of everyToken) expect(t._handle === 0, 'closed token is zeroed');
for (const t of everyToken) t.close();
try {
  holder.primary.value();
  expect(false, 'expected throw for use after close');
} catch (e) {
  expect(e instanceof WeaveFFIError && e.code === -3, `use after close is code -3 (got ${e && e.code})`);
}
try {
  wv.sumHolder(holder);
  expect(false, 'expected throw for encoding a closed object');
} catch (e) {
  expect(e instanceof WeaveFFIError && e.code === -3, `closed object in a buffer is code -3 (got ${e && e.code})`);
}
// The module is unaffected by those consumer-side failures.
const fresh = wv.makeHolder(1n, false);
expect(wv.sumHolder(fresh) === 1n + 3n + 4n + 5n, 'module usable after closed-object errors');
for (const t of [fresh.primary, ...fresh.many]) t.close();
const disposeSym = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');
const disposable = new Token(1n);
expect(typeof disposable[disposeSym] === 'function', 'Token implements Symbol.dispose');
disposable[disposeSym]();
expect(disposable._handle === 0, 'Symbol.dispose zeroes the handle');
disposable[disposeSym]();

if (failures > 0) {
  console.error(`wasm/codec: ${failures} failure(s)`);
  process.exit(1);
}
console.log('wasm/codec: OK');
