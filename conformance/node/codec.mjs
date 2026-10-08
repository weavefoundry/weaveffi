// Conformance consumer: codec sample, the wire oracle (node and wasm lanes).
//
// The shared-vector loop: for every vector the producer serves, decode it,
// hand it back to `checkVector` (which re-encodes it), and push each
// primitive vector's value through the matching direct-family `echo*`. Then
// vectors built from literals (so a symmetric encode/decode bug can't hide),
// spot checks of decoded fields (64-bit integers exact as bigints, float
// bits, astral text), the typed out-of-range error and its payload, the
// marshalling failures the API can express, and object identity and
// reference counting through buffers. Ends with every leak counter at zero.

import { expect, finish, load, same, throws } from './harness.mjs';

const api = await load('codec');
const { codec, CodecError } = api;
const { Color, Token } = codec;

const I64_MIN = -(2n ** 63n);
const I64_MAX = 2n ** 63n - 1n;
const U64_MAX = 2n ** 64n - 1n;

expect(api.__debugLive(-1) === 1n, 'the sample counts live resources');

const n = codec.vectorCount();
expect(n >= 60, `at least 60 vectors (got ${n})`);
const names = Array.from({ length: n }, (_, i) => codec.vectorName(i));

/** The index of the vector named `name`. */
function find(name) {
  const i = names.indexOf(name);
  if (i < 0) throw new Error(`no vector named ${name}`);
  return i;
}

/** Close every object wrapper inside a decoded value. */
function release(v) {
  if (v instanceof Token) {
    v.close();
  } else if (Array.isArray(v)) {
    v.forEach(release);
  } else if (v !== null && typeof v === 'object' && !(v instanceof Uint8Array)) {
    Object.values(v).forEach(release);
  }
}

const f32Bits = (x) => {
  const view = new DataView(new ArrayBuffer(4));
  view.setFloat32(0, x, true);
  return view.getUint32(0, true);
};

// The direct-family echo of each primitive vector, and how to compare.
const echoes = {
  I8: codec.echoI8,
  U8: codec.echoU8,
  I16: codec.echoI16,
  U16: codec.echoU16,
  I32: codec.echoI32,
  U32: codec.echoU32,
  I64: codec.echoI64,
  U64: codec.echoU64,
  F32: codec.echoF32,
  F64: codec.echoF64,
  Flag: codec.echoBool,
  Text: codec.echoText,
  Blob: codec.echoBlob,
  Hue: codec.echoColor,
};

// 1. The loop.
for (let i = 0; i < n; i++) {
  const v = codec.vector(i);
  if (!codec.checkVector(i, v)) {
    expect(false, `vector ${i} (${names[i]}) did not round-trip; producer saw ${codec.describeVector(v)}`);
  }
  expect(!codec.checkVector((i + 1) % n, v), `vector ${i} (${names[i]}) never matches its neighbor`);
  const echo = echoes[v.tag];
  if (echo !== undefined) {
    // Object.is: NaN is NaN, and the sign of zero counts.
    same(echo(v.value), v.value, `echo of vector ${i} (${names[i]})`);
    if (v.tag === 'F32') expect(f32Bits(echo(v.value)) === f32Bits(v.value), `f32 bits of ${names[i]}`);
  }
  release(v);
}

// 2. Literal vectors.
const canonical = {
  i8_value: -8,
  u8_value: 200,
  i16_value: -16000,
  u16_value: 60000,
  i32_value: -2000000000,
  u32_value: 4000000000,
  i64_value: -9007199254740993n,
  u64_value: U64_MAX,
  f32_value: 1.5,
  f64_value: -2.25e100,
  flag: true,
  color: Color.Blue,
};
expect(codec.checkVector(find('scalars canonical'), { tag: 'AllScalars', value: canonical }), 'scalars canonical');
expect(
  !codec.checkVector(find('scalars canonical'), { tag: 'AllScalars', value: { ...canonical, u16_value: 60001 } }),
  'scalars canonical with one field changed',
);
expect(
  codec.checkVector(find('shape labeled'), { tag: 'Figure', value: { tag: 'Labeled', label: 'tag', count: 3 } }),
  'shape labeled',
);
expect(codec.checkVector(find('string interior nul'), { tag: 'Text', value: 'nul\0inside\0' }), 'string interior nul');
const nanView = new DataView(new ArrayBuffer(8));
nanView.setBigUint64(0, 0x7ff8000000000001n, true);
expect(codec.checkVector(find('f64 nan'), { tag: 'F64', value: nanView.getFloat64(0, true) }), 'any NaN is the NaN vector');
expect(codec.checkVector(find('f64 -0'), { tag: 'F64', value: -0 }), 'negative zero');
expect(!codec.checkVector(find('f64 -0'), { tag: 'F64', value: 0 }), 'positive zero is not negative zero');
expect(codec.checkVector(find('u64 max'), { tag: 'U64', value: U64_MAX }), 'u64 max');
expect(codec.checkVector(find('enum infrared'), { tag: 'Hue', value: Color.Infrared }), 'enum infrared');
expect(Color.Infrared === -1 && Color[-1] === 'Infrared', 'a negative enum value maps both ways');
expect(codec.checkVector(find('optional zero'), { tag: 'MaybeI64', value: 0n }), 'optional zero');
expect(codec.checkVector(find('optional absent'), { tag: 'MaybeI64', value: null }), 'optional absent');
expect(!codec.checkVector(find('optional zero'), { tag: 'MaybeI64', value: null }), 'absent is not zero');
expect(
  codec.checkVector(find('map of strings'), { tag: 'Counts', value: { x: 0n, 'héllo': -1n, '': I64_MAX } }),
  'a map built in another order',
);
expect(
  codec.checkVector(
    find('map of strings'),
    { tag: 'Counts', value: new Map([['héllo', -1n], ['', I64_MAX], ['x', 0n]]) },
  ),
  'a Map is a map too',
);
expect(codec.checkVector(find('blank'), { tag: 'Blank' }), 'blank');
const lone = new Token(-1n);
expect(
  codec.checkVector(find('objects sparse'), { tag: 'Objects', value: { primary: lone, spare: null, many: [], by_name: {} } }),
  'objects sparse from a consumer-made token',
);
lone.close();

// 3. Spot checks on decoded values.
same(codec.vector(find('i64 past 2^53')), { tag: 'I64', value: -9007199254740993n }, 'i64 past 2^53 is exact');
const subnormal = codec.vector(find('f32 min subnormal'));
expect(subnormal.tag === 'F32' && f32Bits(subnormal.value) === 1, 'f32 min subnormal bits');
same(codec.vector(find('string astral')), { tag: 'Text', value: '🦀 crab 😀' }, 'string astral');
const minimum = codec.vector(find('scalars minimum')).value;
expect(
  minimum.i8_value === -128 &&
    minimum.i16_value === -32768 &&
    minimum.i32_value === -2147483648 &&
    minimum.i64_value === I64_MIN &&
    minimum.u64_value === 0n,
  'scalars minimum integers',
);
expect(minimum.f32_value === -Infinity && Number.isNaN(minimum.f64_value), 'scalars minimum floats');
expect(minimum.color === Color.Infrared && minimum.flag === false, 'scalars minimum enum and flag');
const deep = codec.vector(find('composite canonical'));
expect(deep.tag === 'Deep', 'composite canonical is Deep');
const c = deep.value;
expect(c.name === 'héllo wörld ✓', 'composite name');
expect(c.blob instanceof Uint8Array && c.blob.length === 6 && c.blob[5] === 255, 'composite blob');
expect(c.some_i64 === I64_MIN && c.none_i64 === null && c.some_text === '', 'composite optionals');
expect(c.names.length === 3 && c.names[1] === '', 'composite names');
expect(c.matrix.length === 3 && c.matrix[1].length === 0 && c.matrix[2][0] === -4, 'composite matrix');
expect(
  c.floats.length === 6 && Number.isNaN(c.floats[0]) && (Object.is(c.floats[3], -0) || c.floats[3] < 0),
  'composite floats',
);
expect(
  Object.keys(c.by_name).length === 4 &&
    Object.keys(c.by_id).length === 3 &&
    Object.keys(c.by_color).length === 2 &&
    Object.keys(c.flags).length === 2,
  'composite maps',
);
expect(c.scalars.u32_value === 4000000000, 'composite scalars');
expect(c.shape.tag === 'Labeled' && c.shape.count === 3, 'composite shape');
expect(c.shapes.length === 6 && c.shapes[5].tag === 'Nested' && c.shapes[5].note === null, 'composite shapes');
expect(c.maybe_shape !== null && c.maybe_shape.tag === 'Nested', 'composite maybe_shape');
expect(c.maybe_list instanceof Uint8Array && c.maybe_list.length === 2, 'composite maybe_list');
expect(c.sparse.length === 3 && c.sparse[0] === true && c.sparse[1] === null, 'composite sparse');
expect(c.colors.length === 4 && c.colors[3] === Color.Infrared, 'composite colors');

// 4. The typed error with its payload.
throws(
  () => codec.vector(n),
  (e) =>
    e instanceof codec.OutOfRangeError &&
    e instanceof codec.CodecError &&
    e instanceof CodecError &&
    e.code === 1 &&
    e.index === n &&
    e.count === n &&
    e.message === `vector ${n} is out of range (count ${n})`,
  'vector past the end',
);
throws(
  () => codec.vectorName(n + 5),
  (e) => e instanceof codec.OutOfRangeError && e.index === n + 5 && e.count === n,
  'vectorName past the end',
);
expect(!codec.checkVector(n, { tag: 'Blank' }), 'checkVector past the end is false');

// 5. Malformed input: an undeclared enum value reaches the producer (and
// fails the non-throwing call as a marshalling trap); the encoder refuses
// what the wire format can't carry.
throws(
  () => codec.echoColor(3),
  (e) => e instanceof CodecError && !(e instanceof codec.CodecError) && e.code === -3,
  'an undeclared enum value',
);
throws(() => codec.checkVector(0, { tag: 'Flag', value: 2 }), (e) => e instanceof TypeError, 'a non-boolean bool');
throws(() => codec.checkVector(0, { tag: 'Nope' }), (e) => e instanceof TypeError, 'an unknown tag');
throws(() => codec.checkVector(0, { tag: 'I64', value: 2n ** 64n }), (e) => e instanceof RangeError, 'an i64 out of range');
throws(() => codec.echoText(null), (e) => e instanceof TypeError, 'a null string');

// 6. Objects.
const full = codec.vector(find('objects full'));
expect(full.tag === 'Objects', 'objects full is Objects');
const h = full.value;
expect(h.primary instanceof Token && h.primary.value() === 10n, 'primary token');
expect(h.spare !== null && h.spare.value() === 11n, 'spare token');
expect(h.many.length === 3 && h.many[2].value() === I64_MIN, 'many tokens');
expect(Object.keys(h.by_name).length === 2 && h.by_name.b.value() === 21n, 'tokens by name');
// Each encoding mints fresh references, so the holder can be sent twice.
const expected = BigInt.asIntN(64, 10n + 11n + 12n + 13n + 20n + 21n + I64_MIN);
expect(codec.sumHolder(h) === expected && codec.sumHolder(h) === expected, 'sumHolder twice');
// primaryOf returns the very same object (a new wrapper of it).
const p = codec.primaryOf(h);
const holder = (primary) => ({ primary, spare: null, many: [], by_name: {} });
expect(codec.samePrimary(h, holder(p)), 'primaryOf is the same object');
const twin = new Token(10n);
expect(!codec.samePrimary(h, holder(twin)), 'an equal value is not the same object');
// A holder of consumer-made tokens, one wrapper in every slot.
const minus4 = new Token(-4n);
expect(
  codec.sumHolder({ primary: twin, spare: twin, many: [twin, twin, minus4], by_name: { k: twin } }) === 46n,
  'a holder of consumer tokens',
);
for (const t of [p, twin, minus4]) t.close();
release(full);
throws(() => p.value(), (e) => e instanceof CodecError && e.code === -3, 'a closed token');

await finish(api, `codec (${n} vectors)`);
