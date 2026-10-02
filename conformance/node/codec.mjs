// Conformance consumer: codec sample (node and wasm lanes).
//
// The value-buffer codec against the producer's oracle in both directions:
// `sample*` fixtures decoded field by field, `verify*` accepting them back,
// and `roundtrip*` returning consumer-built edge cases (empty and unicode
// strings, strings with an interior NUL, 64-bit extremes, NaN, the
// infinities and -0, every Shape variant), the typed MismatchError, range
// and type checks, and objects inside buffers through `Holder`.

import { expect, finish, load, same, throws } from './harness.mjs';

const api = await load('codec');
const { codec, CodecError } = api;
const { Color } = codec;

const bytes = (...xs) => new Uint8Array(xs);

function mismatch(fn, msg) {
  throws(
    fn,
    (e) => e instanceof codec.MismatchError && e instanceof codec.CodecError && e instanceof CodecError && e.code === 1,
    msg,
  );
}

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

function scalars() {
  expect(Color.Red === 0 && Color.Blue === 7 && Color[7] === 'Blue', 'Color values');
  const s = codec.sampleScalars();
  same(s, canonicalScalars, 'sampleScalars');
  expect(codec.verifyScalars(s) && codec.verifyScalars(canonicalScalars), 'verifyScalars');
  same(codec.roundtripScalars(canonicalScalars), canonicalScalars, 'roundtripScalars');
  mismatch(() => codec.verifyScalars({ ...canonicalScalars, u64_value: 18446744073709551614n }), 'u64 off by one');
  mismatch(() => codec.verifyScalars({ ...canonicalScalars, flag: false }), 'flag flipped');
  const back = codec.roundtripScalars(extremes);
  same(back, extremes, 'extreme scalars');
  expect(Object.is(back.f32_value, -0) && Number.isNaN(back.f64_value), '-0 and NaN survive');
  same(codec.roundtripScalars(maxes), maxes, 'max scalars');
  same(
    codec.roundtripScalars({ ...extremes, i64_value: 42, u64_value: 7 }),
    { ...extremes, i64_value: 42n, u64_value: 7n },
    'integral numbers widen to bigint',
  );
  throws(() => codec.roundtripScalars({ ...extremes, u64_value: -1n }), (e) => e instanceof RangeError, 'an out-of-range bigint field');
  throws(() => codec.roundtripScalars({ ...extremes, i32_value: '1' }), (e) => e instanceof TypeError, 'a string for a number field');
  throws(() => codec.roundtripScalars({ ...extremes, flag: 1 }), (e) => e instanceof TypeError, 'a number for a bool field');

  for (const v of [0n, -1n, 9007199254740993n, 9223372036854775807n, -9223372036854775808n]) {
    expect(codec.roundtripI64(v) === v, `roundtripI64(${v})`);
  }
  expect(codec.roundtripI64(5) === 5n, 'roundtripI64 accepts an integral number');
  for (const v of [0n, 9223372036854775808n, 18446744073709551615n]) {
    expect(codec.roundtripU64(v) === v, `roundtripU64(${v})`);
  }
  for (const bad of [9223372036854775808n, -9223372036854775809n]) {
    throws(() => codec.roundtripI64(bad), (e) => e instanceof RangeError, `roundtripI64(${bad})`);
  }
  throws(() => codec.roundtripU64(-1n), (e) => e instanceof RangeError, 'roundtripU64(-1n)');
  throws(() => codec.roundtripI64(1.5), (e) => e instanceof TypeError, 'a fraction for an i64');
  for (const v of [0, -0, 1.5, 5e-324, Number.MAX_VALUE, Infinity, -Infinity]) {
    expect(Object.is(codec.roundtripF64(v), v), `roundtripF64(${v})`);
  }
  expect(Number.isNaN(codec.roundtripF64(NaN)), 'roundtripF64(NaN)');
  expect(codec.roundtripBool(true) === true && codec.roundtripBool(false) === false, 'roundtripBool');
  throws(() => codec.roundtripBool(1), (e) => e instanceof TypeError, 'a number for a bool');
  expect(codec.roundtripColor(Color.Blue) === 7, 'roundtripColor');
}

function strings() {
  for (const s of ['', 'ascii', 'héllo wörld ✓', '日本語', 'emoji 🎉 pair', 'nul\0inside\0', '\0', 'a'.repeat(70000)]) {
    expect(codec.roundtripString(s) === s, `roundtripString(${JSON.stringify(s.slice(0, 20))})`);
  }
  throws(() => codec.roundtripString(5), (e) => e instanceof TypeError, 'a number for a string');
  const all = new Uint8Array(256).map((_, i) => i);
  same(codec.roundtripBytes(all), all, 'roundtripBytes(0..255)');
  const empty = codec.roundtripBytes(new Uint8Array(0));
  expect(empty instanceof Uint8Array && empty.length === 0, 'empty bytes');
  same(codec.roundtripBytes(all.subarray(10, 20)), all.slice(10, 20), 'a Uint8Array view');
  throws(() => codec.roundtripBytes([1, 2]), (e) => e instanceof TypeError, 'an array for bytes');
  expect(codec.roundtripOptI64(null) === null && codec.roundtripOptI64(undefined) === null, 'absent optional');
  expect(codec.roundtripOptI64(0n) === 0n, 'present zero');
  same(codec.roundtripMap({}), {}, 'empty map');
  same(codec.roundtripMap({ a: 1n, '': -2n, 'ключ': 9223372036854775807n }), { a: 1n, '': -2n, 'ключ': 9223372036854775807n }, 'odd keys');
  same(codec.roundtripMap(new Map([['k', 3n]])), { k: 3n }, 'a Map argument');
}

function shapes() {
  const cases = [
    { tag: 'Empty' },
    { tag: 'Circle', radius: 2.5 },
    { tag: 'Circle', radius: -0 },
    { tag: 'Rect', width: 1, height: 0.5 },
    { tag: 'Labeled', label: 'tag', count: 3 },
    { tag: 'Labeled', label: '', count: -2147483648 },
    { tag: 'Nested', inner: canonicalScalars, note: 'n' },
    { tag: 'Nested', inner: extremes, note: null },
  ];
  for (const s of cases) same(codec.roundtripShape(s), s, `roundtripShape(${s.tag})`);
  same(codec.roundtripShapes(cases), cases, 'roundtripShapes');
  expect(codec.describeShape({ tag: 'Circle', radius: 2.5 }) === 'Circle { radius: 2.5 }', 'describeShape');
  throws(() => codec.roundtripShape({ tag: 'Hexagon' }), (e) => e instanceof TypeError, 'an unknown tag');
}

function composites() {
  const canonical = {
    name: 'héllo wörld ✓',
    blob: bytes(0, 1, 2, 253, 254, 255),
    some_i64: -9223372036854775808n,
    none_i64: null,
    some_text: '',
    names: ['a', '', 'ccc'],
    matrix: [[1, 2, 3], [], [-4]],
    empty: [],
    by_name: { one: 1n, two: 2n, neg: -3n },
    by_id: { '-1': canonicalScalars, 42: { ...canonicalScalars, flag: false } },
    scalars: canonicalScalars,
    shape: { tag: 'Labeled', label: 'tag', count: 3 },
    shapes: [
      { tag: 'Empty' },
      { tag: 'Circle', radius: 2.5 },
      { tag: 'Rect', width: 1, height: 0.5 },
      { tag: 'Labeled', label: '', count: -1 },
      { tag: 'Nested', inner: canonicalScalars, note: 'n' },
    ],
    maybe_shape: { tag: 'Nested', inner: canonicalScalars, note: null },
    maybe_list: bytes(9, 8),
    sparse: [true, null, false],
    colors: [Color.Red, Color.Green, Color.Blue],
  };
  const c = codec.sampleComposite();
  same(c, canonical, 'sampleComposite');
  expect(codec.verifyComposite(c) && codec.verifyComposite(canonical), 'verifyComposite');
  same(codec.roundtripComposite(c), canonical, 'roundtripComposite');
  expect(codec.describeComposite(c).startsWith('Composite {'), 'describeComposite');
  mismatch(() => codec.verifyComposite({ ...canonical, sparse: [true, true, false] }), 'sparse changed');
  mismatch(() => codec.verifyComposite({ ...canonical, by_name: { one: 1n, two: 2n } }), 'map entry missing');
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
    by_id: { '-2147483648': extremes, 0: maxes },
    scalars: extremes,
    shape: { tag: 'Empty' },
    shapes: [],
    maybe_shape: null,
    maybe_list: null,
    sparse: [null, null],
    colors: [],
  };
  same(codec.roundtripComposite(edge), edge, 'edge composite');
  const big = { ...edge, names: Array.from({ length: 1000 }, (_, i) => 'name-' + i), blob: new Uint8Array(70000).fill(7) };
  same(codec.roundtripComposite(big), big, 'a large composite');
}

function holders() {
  const holder = codec.makeHolder(10n, true);
  expect(holder.primary instanceof codec.Token && holder.primary.value() === 10n, 'an object field');
  expect(holder.spare.value() === 11n && holder.many.length === 3, 'optional and list object fields');
  expect(codec.sumHolder(holder) === 60n && codec.sumHolder(holder) === 60n, 'encoding clones each object');
  const primary = codec.primaryOf(holder);
  expect(primary !== holder.primary && primary.value() === 10n, 'a new wrapper over the same object');
  expect(codec.samePrimary(holder, { primary, spare: null, many: [] }), 'samePrimary');
  const other = codec.makeHolder(10n, false);
  expect(!codec.samePrimary(holder, other) && other.spare === null, 'distinct objects');
  const t1 = new codec.Token(100n);
  const t2 = new codec.Token(-9223372036854775808n);
  expect(codec.sumHolder({ primary: t1, spare: t2, many: [t1, t1] }) === 300n - 9223372036854775808n, 'consumer-built objects');
  for (const t of [holder.primary, holder.spare, ...holder.many, primary, other.primary, ...other.many, t1, t2]) {
    t.close();
  }
  throws(() => holder.primary.value(), (e) => e instanceof CodecError && e.code === -3, 'use after close');
  throws(() => codec.sumHolder(holder), (e) => e instanceof CodecError && e.code === -3, 'a closed object in a buffer');
  // A holder dropped without closing its objects is released by the GC.
  codec.makeHolder(1n, true);
}

scalars();
strings();
shapes();
composites();
holders();
await finish(api, 'codec');
