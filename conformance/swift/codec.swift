// Conformance consumer: codec sample, Swift target.
//
// Binds through the generated `Codec` module and checks the value-buffer
// codec the Swift backend ships in both directions. `sample*` fixtures the
// producer encodes are decoded and checked field by field against the
// canonical values in `samples/codec/src/lib.rs`; `verify*` sends them back
// (the producer decodes what Swift encoded and compares against its own
// canonical value); `roundtrip*` returns the argument unchanged so consumer
// encoder and decoder are checked against each other, including a `Scalars`
// and a `Composite` built from scratch with edge values (empty strings, lists,
// and maps; unicode; Int64/UInt64 extremes; NaN, infinities, and negative
// zero). `Shape` covers every rich-enum variant, and `Holder` covers object
// tokens inside buffers: a field, an optional, and a list, each carrying one
// strong reference that the wrapper's deinit releases. Exits non-zero on any
// mismatch.

import Foundation
import Codec

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

// --- Equality helpers (the generated structs and rich enum are not Equatable).

func scalarsEqual(_ a: Scalars, _ b: Scalars) -> Bool {
    a.i8Value == b.i8Value && a.u8Value == b.u8Value
        && a.i16Value == b.i16Value && a.u16Value == b.u16Value
        && a.i32Value == b.i32Value && a.u32Value == b.u32Value
        && a.i64Value == b.i64Value && a.u64Value == b.u64Value
        && a.f32Value.bitPattern == b.f32Value.bitPattern
        && a.f64Value.bitPattern == b.f64Value.bitPattern
        && a.flag == b.flag && a.color == b.color
}

func shapeEqual(_ a: Shape, _ b: Shape) -> Bool {
    switch (a, b) {
    case (.empty, .empty):
        return true
    case let (.circle(r1), .circle(r2)):
        return r1.bitPattern == r2.bitPattern
    case let (.rect(w1, h1), .rect(w2, h2)):
        return w1.bitPattern == w2.bitPattern && h1.bitPattern == h2.bitPattern
    case let (.labeled(l1, c1), .labeled(l2, c2)):
        return l1 == l2 && c1 == c2
    case let (.nested(i1, n1), .nested(i2, n2)):
        return scalarsEqual(i1, i2) && n1 == n2
    default:
        return false
    }
}

func shapesEqual(_ a: [Shape], _ b: [Shape]) -> Bool {
    a.count == b.count && zip(a, b).allSatisfy { shapeEqual($0, $1) }
}

func optionalShapeEqual(_ a: Shape?, _ b: Shape?) -> Bool {
    switch (a, b) {
    case (nil, nil): return true
    case let (x?, y?): return shapeEqual(x, y)
    default: return false
    }
}

func compositeEqual(_ a: Composite, _ b: Composite) -> Bool {
    guard a.name == b.name, a.blob == b.blob, a.someI64 == b.someI64, a.noneI64 == b.noneI64,
          a.someText == b.someText, a.names == b.names, a.matrix == b.matrix,
          a.empty.map({ $0.bitPattern }) == b.empty.map({ $0.bitPattern }),
          a.byName == b.byName, a.byId.count == b.byId.count,
          scalarsEqual(a.scalars, b.scalars), shapeEqual(a.shape, b.shape),
          shapesEqual(a.shapes, b.shapes), optionalShapeEqual(a.maybeShape, b.maybeShape),
          a.maybeList == b.maybeList, a.sparse == b.sparse, a.colors == b.colors
    else { return false }
    for (k, v) in a.byId {
        guard let w = b.byId[k], scalarsEqual(v, w) else { return false }
    }
    return true
}

// --- Scalars ------------------------------------------------------------------

let canonical = Scalars(
    i8Value: -8, u8Value: 200, i16Value: -16_000, u16Value: 60_000,
    i32Value: -2_000_000_000, u32Value: 4_000_000_000,
    i64Value: -9_007_199_254_740_993, u64Value: UInt64.max,
    f32Value: 1.5, f64Value: -2.25e100, flag: true, color: .blue)

let scalars = Codec.sampleScalars()
expect(scalars.i8Value == -8, "i8 (got \(scalars.i8Value))")
expect(scalars.u8Value == 200, "u8 (got \(scalars.u8Value))")
expect(scalars.i16Value == -16_000, "i16 (got \(scalars.i16Value))")
expect(scalars.u16Value == 60_000, "u16 (got \(scalars.u16Value))")
expect(scalars.i32Value == -2_000_000_000, "i32 (got \(scalars.i32Value))")
expect(scalars.u32Value == 4_000_000_000, "u32 (got \(scalars.u32Value))")
expect(scalars.i64Value == -9_007_199_254_740_993, "i64 (got \(scalars.i64Value))")
expect(scalars.u64Value == UInt64.max, "u64 (got \(scalars.u64Value))")
expect(scalars.f32Value == 1.5, "f32 (got \(scalars.f32Value))")
expect(scalars.f64Value == -2.25e100, "f64 (got \(scalars.f64Value))")
expect(scalars.flag == true, "flag")
expect(scalars.color == .blue, "color (got \(scalars.color))")
expect(scalarsEqual(scalars, canonical), "sampleScalars matches the canonical fixture")

do {
    expect(try Codec.verifyScalars(value: scalars) == true, "producer accepts the re-encoded sample")
    expect(try Codec.verifyScalars(value: canonical) == true, "producer accepts a locally built canonical")
} catch {
    fail("verifyScalars threw: \(error)")
}
expect(scalarsEqual(Codec.roundtripScalars(value: scalars), scalars), "roundtripScalars is the identity")

// A single changed field is a typed CodecError.mismatch (code 1).
var tweaked = canonical
tweaked.u64Value -= 1
do {
    _ = try Codec.verifyScalars(value: tweaked)
    fail("expected CodecError.mismatch for a tweaked Scalars")
} catch let e as CodecError {
    guard case let .mismatch(message) = e else { fail("expected .mismatch, got \(e)") }
    expect(e.errorCode == 1, "mismatch code == 1 (got \(e.errorCode))")
    expect(message == "value does not match the canonical fixture", "mismatch message (got \(message))")
} catch {
    fail("expected CodecError, got \(error)")
}

// Scalars built from scratch at the extremes, with non-finite floats and a
// negative zero, survive a round trip bit for bit.
let extremes = Scalars(
    i8Value: Int8.min, u8Value: UInt8.max, i16Value: Int16.max, u16Value: UInt16.max,
    i32Value: Int32.min, u32Value: UInt32.max, i64Value: Int64.min, u64Value: 1 << 63,
    f32Value: -0.0, f64Value: Double.nan, flag: false, color: .red)
let extremesBack = Codec.roundtripScalars(value: extremes)
expect(extremesBack.i64Value == Int64.min && extremesBack.u64Value == 1 << 63, "64-bit extremes")
expect(extremesBack.f32Value == 0 && extremesBack.f32Value.sign == .minus, "negative zero f32 keeps its sign")
expect(extremesBack.f64Value.isNaN, "NaN f64 round-trips")
expect(scalarsEqual(extremesBack, extremes), "extreme Scalars round-trip bit for bit")
let infinities = Scalars(
    i8Value: Int8.max, u8Value: 0, i16Value: Int16.min, u16Value: 0, i32Value: Int32.max, u32Value: 0,
    i64Value: Int64.max, u64Value: 0, f32Value: Float.infinity, f64Value: -Double.infinity,
    flag: true, color: .green)
expect(scalarsEqual(Codec.roundtripScalars(value: infinities), infinities), "infinities round-trip")

// --- Composite ----------------------------------------------------------------

let composite = Codec.sampleComposite()
expect(composite.name == "héllo wörld ✓", "name (got \(composite.name))")
expect(composite.blob == Data([0, 1, 2, 253, 254, 255]), "blob (got \(Array(composite.blob)))")
expect(composite.someI64 == Int64.min, "someI64 (got \(String(describing: composite.someI64)))")
expect(composite.noneI64 == nil, "noneI64 nil")
expect(composite.someText == "", "someText is a present empty string (got \(String(describing: composite.someText)))")
expect(composite.names == ["a", "", "ccc"], "names (got \(composite.names))")
expect(composite.matrix == [[1, 2, 3], [], [-4]], "matrix (got \(composite.matrix))")
expect(composite.empty.isEmpty, "empty list")
expect(composite.byName == ["one": 1, "two": 2, "neg": -3], "byName (got \(composite.byName))")
expect(composite.byId.count == 2, "byId has two entries (got \(composite.byId.count))")
expect(composite.byId[-1].map { scalarsEqual($0, canonical) } == true, "byId[-1] is the canonical Scalars")
expect(composite.byId[42]?.flag == false, "byId[42] has flag false")
var flagless = canonical
flagless.flag = false
expect(composite.byId[42].map { scalarsEqual($0, flagless) } == true, "byId[42] otherwise canonical")
expect(scalarsEqual(composite.scalars, canonical), "nested scalars")
expect(shapeEqual(composite.shape, .labeled(label: "tag", count: 3)), "shape (got \(composite.shape))")
expect(composite.shapes.count == 5, "five shapes (got \(composite.shapes.count))")
expect(shapesEqual(composite.shapes, [
    .empty,
    .circle(radius: 2.5),
    .rect(width: 1.0, height: 0.5),
    .labeled(label: "", count: -1),
    .nested(inner: canonical, note: "n"),
]), "shapes one of each variant (got \(composite.shapes))")
expect(optionalShapeEqual(composite.maybeShape, .nested(inner: canonical, note: nil)),
       "maybeShape is a nested variant with an absent note (got \(String(describing: composite.maybeShape)))")
expect(composite.maybeList == Data([9, 8]), "maybeList (got \(String(describing: composite.maybeList)))")
expect(composite.sparse == [true, nil, false], "sparse (got \(composite.sparse))")
expect(composite.colors == [.red, .green, .blue], "colors (got \(composite.colors))")

do {
    expect(try Codec.verifyComposite(value: composite) == true, "producer accepts the re-encoded composite")
} catch {
    fail("verifyComposite threw: \(error); producer saw: \(Codec.describeComposite(value: composite))")
}
expect(compositeEqual(Codec.roundtripComposite(value: composite), composite), "roundtripComposite is the identity")
let described = Codec.describeComposite(value: composite)
expect(described.hasPrefix("Composite {") && described.contains("héllo wörld ✓"), "describeComposite (got \(described))")

// A changed nested value is rejected.
var changed = composite
changed.sparse[1] = true
do {
    _ = try Codec.verifyComposite(value: changed)
    fail("expected CodecError.mismatch for a changed composite")
} catch let e as CodecError {
    expect(e.errorCode == 1, "composite mismatch code == 1")
} catch {
    fail("expected CodecError, got \(error)")
}

// A Composite built from scratch with edge values: empty text, bytes, lists,
// and maps; unicode beyond the BMP; present-but-empty optionals; and
// non-finite, negative-zero, and subnormal floats inside nested positions.
let homemade = Composite(
    name: "",
    blob: Data(),
    someI64: Int64.max,
    noneI64: -1,
    someText: nil,
    names: ["日本語", "🚀 rocket", "", "\u{0}nul"],
    matrix: [[], [Int32.min, Int32.max], []],
    empty: [Double.nan, -0.0, Double.infinity, -Double.infinity, Double.leastNonzeroMagnitude],
    byName: [:],
    byId: [Int32.min: extremes, 0: canonical, Int32.max: infinities],
    scalars: extremes,
    shape: .empty,
    shapes: [],
    maybeShape: nil,
    maybeList: Data(),
    sparse: [nil, nil, true],
    colors: [.blue, .blue, .red, .green])
let homemadeBack = Codec.roundtripComposite(value: homemade)
expect(homemadeBack.name.isEmpty && homemadeBack.blob.isEmpty, "empty string and bytes")
expect(homemadeBack.someText == nil, "absent optional string")
expect(homemadeBack.maybeList != nil && homemadeBack.maybeList!.isEmpty, "present empty optional bytes")
expect(homemadeBack.names == homemade.names, "unicode names (got \(homemadeBack.names))")
expect(homemadeBack.matrix == [[], [Int32.min, Int32.max], []], "matrix with empty rows")
expect(homemadeBack.empty[0].isNaN, "NaN inside a list")
expect(homemadeBack.empty[1] == 0 && homemadeBack.empty[1].sign == .minus, "negative zero inside a list")
expect(homemadeBack.empty[2] == Double.infinity && homemadeBack.empty[3] == -Double.infinity, "infinities inside a list")
expect(homemadeBack.empty[4] == Double.leastNonzeroMagnitude, "subnormal double inside a list")
expect(homemadeBack.byName.isEmpty && homemadeBack.byId.count == 3, "empty map, three-entry map")
expect(homemadeBack.byId[Int32.min].map { scalarsEqual($0, extremes) } == true, "Int32.min key")
expect(homemadeBack.shapes.isEmpty && homemadeBack.maybeShape == nil, "empty shapes, absent maybeShape")
expect(compositeEqual(homemadeBack, homemade), "homemade Composite round-trips")
expect(Codec.describeComposite(value: homemade).contains("日本語"), "describe renders unicode")
do {
    _ = try Codec.verifyComposite(value: homemade)
    fail("homemade composite is not the canonical one")
} catch let e as CodecError {
    expect(e.errorCode == 1, "homemade composite mismatch code == 1")
} catch {
    fail("expected CodecError, got \(error)")
}

// --- Shape (rich enum) ---------------------------------------------------------

let allShapes: [Shape] = [
    .empty,
    .circle(radius: -0.0),
    .rect(width: Float.nan, height: -Float.infinity),
    .labeled(label: "ünïcödé ✓", count: Int32.min),
    .nested(inner: extremes, note: ""),
    .nested(inner: canonical, note: nil),
]
for s in allShapes {
    expect(shapeEqual(Codec.roundtripShape(value: s), s), "roundtripShape identity for \(s)")
}
expect(shapesEqual(Codec.roundtripShapes(value: allShapes), allShapes), "roundtripShapes identity")
expect(Codec.roundtripShapes(value: []).isEmpty, "roundtripShapes of an empty list")
expect(Codec.describeShape(value: .empty) == "Empty", "describe Empty")
expect(Codec.describeShape(value: .circle(radius: 2.5)) == "Circle { radius: 2.5 }", "describe Circle")
expect(Codec.describeShape(value: .rect(width: 1.0, height: 0.5)) == "Rect { width: 1.0, height: 0.5 }",
       "describe Rect")
expect(Codec.describeShape(value: .labeled(label: "tag", count: 3)) == "Labeled { label: \"tag\", count: 3 }",
       "describe Labeled")
expect(Codec.describeShape(value: .nested(inner: canonical, note: nil)).hasPrefix("Nested { inner: Scalars {"),
       "describe Nested")

// --- Direct, string, bytes, and standalone buffered families --------------------

expect(Codec.roundtripOptI64(value: nil) == nil, "optional i64 absent")
expect(Codec.roundtripOptI64(value: Int64.min) == Int64.min, "optional i64 present")
expect(Codec.roundtripOptI64(value: 0) == 0, "optional i64 zero is present, not absent")
expect(Codec.roundtripMap(value: [:]).isEmpty, "empty map")
let bigMap: [String: Int64] = ["": 0, "a": Int64.max, "ключ": Int64.min, "z": -1]
expect(Codec.roundtripMap(value: bigMap) == bigMap, "map with empty and unicode keys")
expect(Codec.roundtripString(value: "") == "", "empty string")
expect(Codec.roundtripString(value: "héllo wörld ✓ 🚀") == "héllo wörld ✓ 🚀", "unicode string")
expect(Codec.roundtripBytes(value: Data()).isEmpty, "empty bytes")
let allBytes = Data((0...255).map { UInt8($0) })
expect(Codec.roundtripBytes(value: allBytes) == allBytes, "every byte value")
expect(Codec.roundtripI64(value: Int64.min) == Int64.min, "i64 min")
expect(Codec.roundtripI64(value: Int64.max) == Int64.max, "i64 max")
expect(Codec.roundtripI64(value: -1) == -1, "i64 -1")
expect(Codec.roundtripU64(value: UInt64.max) == UInt64.max, "u64 max")
expect(Codec.roundtripU64(value: 1 << 63) == 1 << 63, "u64 above 2^63")
expect(Codec.roundtripU64(value: 0) == 0, "u64 zero")
expect(Codec.roundtripF64(value: Double.nan).isNaN, "f64 NaN")
expect(Codec.roundtripF64(value: Double.infinity) == Double.infinity, "f64 +inf")
expect(Codec.roundtripF64(value: -Double.infinity) == -Double.infinity, "f64 -inf")
let negZero = Codec.roundtripF64(value: -0.0)
expect(negZero == 0 && negZero.sign == .minus, "f64 negative zero keeps its sign")
expect(Codec.roundtripF64(value: Double.leastNonzeroMagnitude) == Double.leastNonzeroMagnitude, "f64 subnormal")
expect(Codec.roundtripF64(value: -2.25e100) == -2.25e100, "f64 large negative")
expect(Codec.roundtripBool(value: true) == true && Codec.roundtripBool(value: false) == false, "bool")
expect(Codec.roundtripColor(value: .blue) == .blue, "color blue (discriminant 7)")
expect(Codec.roundtripColor(value: .red) == .red, "color red")

// --- Holder: objects inside buffers -------------------------------------------

let holder = Codec.makeHolder(base: 10, withSpare: true)
expect(holder.primary.value() == 10, "primary token (got \(holder.primary.value()))")
expect(holder.spare?.value() == 11, "spare token (got \(String(describing: holder.spare?.value())))")
expect(holder.many.map { $0.value() } == [12, 13, 14], "many tokens (got \(holder.many.map { $0.value() }))")
// Encoding clones every token; the producer adopts and drops those clones,
// and the wrappers remain valid for the next call.
expect(Codec.sumHolder(holder: holder) == 60, "sumHolder (got \(Codec.sumHolder(holder: holder)))")
expect(Codec.sumHolder(holder: holder) == 60, "sumHolder again: no reference was consumed")
expect(holder.primary.value() == 10 && holder.many[2].value() == 14, "tokens still alive after encoding")

// primaryOf returns the very object stored in holder.primary.
let primary = Codec.primaryOf(holder: holder)
expect(primary.value() == 10, "primaryOf value")
let rebuilt = Holder(primary: primary, spare: nil, many: [])
expect(Codec.samePrimary(a: holder, b: rebuilt), "primaryOf returned the same object as holder.primary")
expect(Codec.samePrimary(a: holder, b: holder), "a holder shares its own primary")
expect(!Codec.samePrimary(a: holder, b: Codec.makeHolder(base: 10, withSpare: true)),
       "a fresh holder with equal values is a different object")
expect(Codec.sumHolder(holder: rebuilt) == 10, "sumHolder over a consumer-built holder")

let bare = Codec.makeHolder(base: 0, withSpare: false)
expect(bare.spare == nil, "withSpare false yields no spare")
expect(bare.primary.value() == 0 && bare.many.map { $0.value() } == [2, 3, 4], "bare tokens")
expect(Codec.sumHolder(holder: bare) == 9, "sumHolder without spare (got \(Codec.sumHolder(holder: bare)))")

// Consumer-created tokens, including one object referenced twice from the
// same buffer (two clones, two adoptions).
let shared = Token(value: 100)
let mine = Holder(primary: shared, spare: Token(value: 5), many: [shared, Token(value: 1), shared])
expect(Codec.sumHolder(holder: mine) == 100 + 5 + 100 + 1 + 100, "sumHolder over shared tokens")
expect(Codec.samePrimary(a: mine, b: Holder(primary: shared, spare: nil, many: [])), "shared primary identity")
expect(shared.value() == 100, "shared token alive after three encodings")
expect(Codec.sumHolder(holder: Holder(primary: Token(value: -7), spare: nil, many: [])) == -7, "minimal holder")

// Release: every wrapper's deinit releases its strong reference, including
// the ones adopted from a decoded buffer and the one returned by primaryOf.
weak var weakPrimary: Token?
weak var weakSpare: Token?
weak var weakMany: Token?
weak var weakReturned: Token?
do {
    let h = Codec.makeHolder(base: 20, withSpare: true)
    weakPrimary = h.primary
    weakSpare = h.spare
    weakMany = h.many[0]
    let p = Codec.primaryOf(holder: h)
    weakReturned = p
    expect(p.value() == 20 && Codec.sumHolder(holder: h) == 20 + 21 + 22 + 23 + 24, "scoped holder usable")
}
expect(weakPrimary == nil && weakSpare == nil && weakMany == nil && weakReturned == nil,
       "every token wrapper was deinitialized when the holder left scope")

print("swift/codec: OK")
