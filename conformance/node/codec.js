// Conformance consumer: codec sample, Node (N-API) target.
//
// Checks the generated value-buffer codec (index.js) against the producer's
// round-trip oracle in both directions: `sample*` fixtures decoded field by
// field against concrete expected values (producer encodes, consumer
// decodes), `verify*` accepting the same fixture back (consumer encodes,
// producer decodes), `roundtrip*` returning consumer-built values with edge
// cases (empty strings, lists, and maps; unicode; i64/u64 extremes as BigInt;
// NaN, +/-Infinity, and -0 doubles; every Shape variant), the typed
// MismatchError, and objects inside buffers through `Holder` (a Token field,
// an optional Token, a list of Tokens; `primaryOf` returning the same object;
// `samePrimary`; every wrapper released with an idempotent `close()`). The
// harness passes the built addon via WV_ADDON; the generated loader honors
// WEAVEFFI_ADDON.

'use strict';

const assert = require('assert');
const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-codec/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/codec/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'codec', 'node', 'index.js')
);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}
// Structural equality with SameValue semantics for primitives: BigInt by
// value, NaN equal to NaN, -0 distinct from +0, Buffer by contents.
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
    expect(false, msg + ': expected MismatchError');
  } catch (e) {
    expect(e instanceof wv.MismatchError, msg + ': MismatchError instance (got ' + (e && e.constructor.name) + ')');
    expect(e instanceof wv.CodecError, msg + ': extends CodecError');
    expect(e instanceof wv.WeaveFFIError, msg + ': extends WeaveFFIError');
    expect(e.code === 1 && wv.MismatchError.CODE === 1, msg + ': code 1');
  }
}

// The C-style enum is exported as a frozen object (forward and reverse
// mappings), the runtime value `types.d.ts` declares as `export enum`.
const Color = wv.Color;
expect(Color.Red === 0 && Color.Green === 1 && Color.Blue === 7, 'Color values');
expect(Color[7] === 'Blue', 'Color reverse mapping');

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
expect(typeof scalars.i32_value === 'number', 'i32 field is a number');
expect(scalars.i64_value === -9007199254740993n, 'i64 beyond 2^53 is exact');
expect(wv.verifyScalars(scalars) === true, 'verifyScalars accepts the decoded fixture');
expect(wv.verifyScalars(canonicalScalars) === true, 'verifyScalars accepts a hand-built fixture');
same(wv.roundtripScalars(scalars), canonicalScalars, 'roundtripScalars preserves every field');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, u64_value: 18446744073709551614n }), 'u64 off by one');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, flag: false }), 'flag flipped');
throwsMismatch(() => wv.verifyScalars({ ...canonicalScalars, color: Color.Red }), 'color changed');

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
same(wv.roundtripScalars({ ...extremes, i64_value: 42, u64_value: 7 }), { ...extremes, i64_value: 42n, u64_value: 7n }, 'number literals widen to BigInt on the way back');

// Direct-family 64-bit params and returns are BigInt end to end.
for (const v of [0n, 1n, -1n, 9007199254740993n, -9007199254740993n, 9223372036854775807n, -9223372036854775808n]) {
  const back = wv.roundtripI64(v);
  expect(typeof back === 'bigint' && back === v, `roundtripI64(${v}) (got ${back})`);
}
expect(wv.roundtripI64(5) === 5n, 'roundtripI64 accepts a number and returns a BigInt');
for (const v of [0n, 1n, 9223372036854775808n, 18446744073709551615n]) {
  const back = wv.roundtripU64(v);
  expect(typeof back === 'bigint' && back === v, `roundtripU64(${v}) (got ${back})`);
}
for (const bad of [9223372036854775808n, -9223372036854775809n]) {
  try {
    wv.roundtripI64(bad);
    expect(false, `roundtripI64(${bad}) should reject an out-of-range BigInt`);
  } catch (e) {
    expect(e instanceof RangeError, `out-of-range i64 is a RangeError (got ${e && e.constructor.name})`);
  }
}
try {
  wv.roundtripU64(-1n);
  expect(false, 'roundtripU64(-1n) should reject');
} catch (e) {
  expect(e instanceof RangeError, `negative u64 is a RangeError (got ${e && e.constructor.name})`);
}

// Doubles by value.
for (const v of [0, -0, 1.5, -2.25e100, 5e-324, Number.MAX_VALUE, Infinity, -Infinity]) {
  expect(Object.is(wv.roundtripF64(v), v), `roundtripF64(${v})`);
}
expect(Number.isNaN(wv.roundtripF64(NaN)), 'roundtripF64(NaN)');
expect(wv.roundtripBool(true) === true && wv.roundtripBool(false) === false, 'roundtripBool');
expect(wv.roundtripColor(Color.Blue) === 7, 'roundtripColor(Blue) == 7');
expect(wv.roundtripColor(Color.Red) === 0, 'roundtripColor(Red) == 0');

// Strings and bytes at the top level.
for (const s of ['', 'ascii', 'héllo wörld ✓', '日本語', 'emoji 🎉 pair', 'a'.repeat(70000)]) {
  expect(wv.roundtripString(s) === s, `roundtripString(${JSON.stringify(s)})`);
}
const allBytes = Buffer.from(Array.from({ length: 256 }, (_, i) => i));
expect(wv.roundtripBytes(allBytes).equals(allBytes), 'roundtripBytes(0..255)');
const emptyBytes = wv.roundtripBytes(Buffer.alloc(0));
expect(Buffer.isBuffer(emptyBytes) && emptyBytes.length === 0, 'roundtripBytes(empty) is an empty Buffer');

// Optionals and maps at the top level.
expect(wv.roundtripOptI64(null) === null, 'roundtripOptI64(null)');
expect(wv.roundtripOptI64(undefined) === null, 'roundtripOptI64(undefined) is null');
expect(wv.roundtripOptI64(-9223372036854775808n) === -9223372036854775808n, 'roundtripOptI64(i64::MIN)');
expect(wv.roundtripOptI64(0n) === 0n, 'roundtripOptI64(0n) is present');
same(wv.roundtripMap({}), {}, 'roundtripMap({})');
same(wv.roundtripMap({ a: 1n, '': -2n, 'ключ': 9223372036854775807n }), { a: 1n, '': -2n, 'ключ': 9223372036854775807n }, 'roundtripMap with odd keys');

// --- Shape (rich enum) ------------------------------------------------------
const shapeCases = [
  { tag: 'Empty' },
  { tag: 'Circle', radius: 2.5 },
  { tag: 'Circle', radius: -0 },
  { tag: 'Rect', width: 1.0, height: 0.5 },
  { tag: 'Labeled', label: 'tag', count: 3 },
  { tag: 'Labeled', label: '', count: -2147483648 },
  { tag: 'Nested', inner: canonicalScalars, note: 'n' },
  { tag: 'Nested', inner: extremes, note: null },
];
for (const s of shapeCases) {
  same(wv.roundtripShape(s), s, `roundtripShape(${s.tag})`);
}
same(wv.roundtripShapes(shapeCases), shapeCases, 'roundtripShapes(all variants)');
same(wv.roundtripShapes([]), [], 'roundtripShapes([])');
expect(wv.describeShape({ tag: 'Empty' }) === 'Empty', 'describeShape(Empty)');
expect(wv.describeShape({ tag: 'Circle', radius: 2.5 }) === 'Circle { radius: 2.5 }', `describeShape(Circle) (got ${wv.describeShape({ tag: 'Circle', radius: 2.5 })})`);
expect(wv.describeShape({ tag: 'Labeled', label: 'tag', count: 3 }) === 'Labeled { label: "tag", count: 3 }', 'describeShape(Labeled)');
const nestedBack = wv.roundtripShape({ tag: 'Nested', inner: extremes, note: null });
expect(Number.isNaN(nestedBack.inner.f64_value) && nestedBack.note === null, 'nested NaN and absent note');
try {
  wv.roundtripShape({ tag: 'Hexagon' });
  expect(false, 'unknown tag should throw');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -2, `unknown tag is a runtime trap (got ${e && e.code})`);
}

// --- Composite --------------------------------------------------------------
const canonicalComposite = {
  name: 'héllo wörld ✓',
  blob: Buffer.from([0, 1, 2, 253, 254, 255]),
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
  maybe_list: Buffer.from([9, 8]),
  sparse: [true, null, false],
  colors: [Color.Red, Color.Green, Color.Blue],
};
const composite = wv.sampleComposite();
same(composite, canonicalComposite, 'sampleComposite matches the canonical fixture');
expect(Buffer.isBuffer(composite.blob), 'blob is a Buffer');
expect(Buffer.isBuffer(composite.maybe_list), 'maybe_list is a Buffer');
expect(typeof composite.some_i64 === 'bigint', 'some_i64 is a BigInt');
expect(typeof composite.by_name.one === 'bigint', 'map values are BigInt');
expect(composite.by_id[42].flag === false && composite.by_id[-1].flag === true, 'i32-keyed map indexes by number');
expect(wv.verifyComposite(composite) === true, 'verifyComposite accepts the decoded fixture');
expect(wv.verifyComposite(canonicalComposite) === true, 'verifyComposite accepts a hand-built fixture');
same(wv.roundtripComposite(composite), canonicalComposite, 'roundtripComposite preserves every field');
const described = wv.describeComposite(composite);
expect(typeof described === 'string' && described.startsWith('Composite {') && described.includes('name: "héllo wörld ✓"'), `describeComposite renders (got ${described.slice(0, 40)}...)`);
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, sparse: [true, true, false] }), 'sparse changed');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, none_i64: 0n }), 'absent optional made present');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, by_name: { one: 1n, two: 2n } }), 'map entry missing');
throwsMismatch(() => wv.verifyComposite({ ...canonicalComposite, name: 'hello world' }), 'unicode name changed');

// A consumer-built composite with edge values in every position.
const edge = {
  name: '',
  blob: Buffer.alloc(0),
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
const bigList = {
  ...edge,
  names: Array.from({ length: 1000 }, (_, i) => 'name-' + i),
  blob: allBytes,
  maybe_list: Buffer.from([255]),
  sparse: Array.from({ length: 333 }, (_, i) => (i % 3 === 0 ? null : i % 3 === 1)),
};
same(wv.roundtripComposite(bigList), bigList, 'large composite round-trips (writer growth)');

// --- Holder: objects inside buffers -----------------------------------------
const holder = wv.makeHolder(10n, true);
expect(holder.primary instanceof wv.Token, 'holder.primary is a Token');
expect(holder.primary.value() === 10n, `primary value 10n (got ${holder.primary.value()})`);
expect(holder.spare instanceof wv.Token && holder.spare.value() === 11n, 'spare is Token(11)');
expect(Array.isArray(holder.many) && holder.many.length === 3, 'many has 3 tokens');
same(holder.many.map((t) => t.value()), [12n, 13n, 14n], 'many values 12..14');
expect(wv.sumHolder(holder) === 60n, `sumHolder == 60n (got ${wv.sumHolder(holder)})`);
// Encoding clones each handle, so the wrappers are still usable afterwards.
expect(holder.primary.value() === 10n && holder.many[2].value() === 14n, 'tokens usable after sumHolder');
expect(wv.sumHolder(holder) === 60n, 'sumHolder again (fresh clones each call)');

// primaryOf returns a new wrapper over the same underlying object.
const primary = wv.primaryOf(holder);
expect(primary instanceof wv.Token && primary !== holder.primary, 'primaryOf returns a distinct wrapper');
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

// Consumer-constructed tokens in every buffered position.
const t1 = new wv.Token(100n);
const t2 = new wv.Token(200);
const t3 = new wv.Token(-9223372036854775808n);
expect(t1.value() === 100n && t2.value() === 200n && t3.value() === -9223372036854775808n, 'constructed token values');
const mine = { primary: t1, spare: t2, many: [t3, t1, t1] };
expect(wv.sumHolder(mine) === 100n + 200n + -9223372036854775808n + 100n + 100n, 'sumHolder over consumer tokens');
expect(wv.sumHolder({ primary: t1, spare: null, many: [] }) === 100n, 'sumHolder with no spare and empty many');
expect(wv.samePrimary(mine, { primary: t1, spare: null, many: [] }) === true, 'same consumer token in two holders');
expect(wv.samePrimary(mine, { primary: t2, spare: null, many: [] }) === false, 'different consumer tokens');
const primaryMine = wv.primaryOf(mine);
expect(primaryMine.value() === 100n, 'primaryOf a consumer holder');

// Release everything: close is idempotent, and a closed token can't be
// encoded (borrow after close traps with -3) or used.
const everyToken = [
  holder.primary, holder.spare, ...holder.many, primary,
  other.primary, other.spare, ...other.many,
  noSpare.primary, ...noSpare.many,
  t1, t2, t3, primaryMine,
];
for (const t of everyToken) t.close();
for (const t of everyToken) t.close();
try {
  holder.primary.value();
  expect(false, 'expected throw for use after close');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -3, `use after close is code -3 (got ${e && e.code})`);
}
try {
  wv.sumHolder(holder);
  expect(false, 'expected throw for encoding a closed object');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -3, `closed object in a buffer is code -3 (got ${e && e.code})`);
}
if (typeof Symbol.dispose === 'symbol') {
  const disposable = new wv.Token(1n);
  disposable[Symbol.dispose]();
  disposable[Symbol.dispose]();
}

if (failures > 0) process.exit(1);
console.log('node/codec: OK');
