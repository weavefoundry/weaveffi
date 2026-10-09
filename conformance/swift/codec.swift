// Conformance consumer: codec sample, Swift target (ABI revision 4).
//
// The shared-vector loop through the generated `Codec` module: every vector
// the producer serves is decoded by the Swift codec, re-encoded on the way
// back into `checkVector` (which must accept it, and reject it for the
// neighboring index), and, for primitive vectors, pushed through the
// matching direct-family `echo*`. Then vectors built from Swift literals (so
// a symmetric encode/decode bug can't hide), spot checks of decoded values,
// the typed `CodecError.outOfRange` with its payload, malformed buffers sent
// through the raw C entry points (the Swift encoder can't produce them), and
// object identity and reference counting through buffers. Ends by asserting
// the producer's leak counters are zero. Exits non-zero on any mismatch.

import CCodec
import Codec
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: @autoclosure () -> String) {
    if !cond { fail(msg()) }
}

/// Every live-resource counter must settle at zero: 0 objects, 1 callbacks,
/// 2 iterators, 3 cancel tokens, 4 allocations.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<2000 {
        live = (0..<5).map { codec_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(1000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

/// Byte-exact string equality (Swift's `==` compares canonical equivalence).
func same(_ a: String, _ b: String) -> Bool {
    Array(a.utf8) == Array(b.utf8)
}

func vector(_ i: UInt32) -> Vector {
    do {
        return try Codec.vector(index: i)
    } catch {
        fail("vector(\(i)) threw \(error)")
    }
}

/// The index of every vector, by name.
func vectorIndex(_ n: UInt32) -> [String: UInt32] {
    var names: [String: UInt32] = [:]
    for i in 0..<n {
        do {
            names[try Codec.vectorName(index: i)] = i
        } catch {
            fail("vectorName(\(i)) threw \(error)")
        }
    }
    return names
}

/// Push a primitive vector's value through its direct-family echo.
func echo(_ v: Vector) {
    switch v {
    case let .i8(x): expect(Codec.echoI8(value: x) == x, "echoI8(\(x))")
    case let .u8(x): expect(Codec.echoU8(value: x) == x, "echoU8(\(x))")
    case let .i16(x): expect(Codec.echoI16(value: x) == x, "echoI16(\(x))")
    case let .u16(x): expect(Codec.echoU16(value: x) == x, "echoU16(\(x))")
    case let .i32(x): expect(Codec.echoI32(value: x) == x, "echoI32(\(x))")
    case let .u32(x): expect(Codec.echoU32(value: x) == x, "echoU32(\(x))")
    case let .i64(x): expect(Codec.echoI64(value: x) == x, "echoI64(\(x))")
    case let .u64(x): expect(Codec.echoU64(value: x) == x, "echoU64(\(x))")
    case let .f32(x):
        expect(Codec.echoF32(value: x).bitPattern == x.bitPattern, "echoF32(\(x)) bit for bit")
    case let .f64(x):
        expect(Codec.echoF64(value: x).bitPattern == x.bitPattern, "echoF64(\(x)) bit for bit")
    case let .flag(x): expect(Codec.echoBool(value: x) == x, "echoBool(\(x))")
    case let .text(x): expect(same(Codec.echoText(value: x), x), "echoText(\(x.debugDescription))")
    case let .blob(x): expect(Codec.echoBlob(value: x) == x, "echoBlob(\(x as NSData))")
    case let .hue(x): expect(Codec.echoColor(value: x) == x, "echoColor(\(x))")
    default: break
    }
}

func everyVector(_ n: UInt32) {
    for i in 0..<n {
        let v = vector(i)
        if !Codec.checkVector(index: i, value: v) {
            let name = (try? Codec.vectorName(index: i)) ?? "?"
            fail("vector \(i) (\(name)) did not round-trip; producer saw \(Codec.describeVector(value: v))")
        }
        expect(!Codec.checkVector(index: (i + 1) % n, value: v), "vector \(i) never matches its neighbor")
        echo(v)
    }
}

let canonicalScalars = Scalars(
    i8Value: -8, u8Value: 200, i16Value: -16_000, u16Value: 60_000,
    i32Value: -2_000_000_000, u32Value: 4_000_000_000,
    i64Value: -9_007_199_254_740_993, u64Value: .max,
    f32Value: 1.5, f64Value: -2.25e100, flag: true, color: .blue)

func literalVectors(_ index: [String: UInt32]) {
    func check(_ name: String, _ v: Vector, _ want: Bool) {
        guard let i = index[name] else { fail("no vector named \(name)") }
        expect(Codec.checkVector(index: i, value: v) == want, "literal vs \(name) should be \(want)")
    }

    check("scalars canonical", .allScalars(value: canonicalScalars), true)
    var tweaked = canonicalScalars
    tweaked.u16Value = 60_001
    check("scalars canonical", .allScalars(value: tweaked), false)
    check("shape labeled", .figure(value: .labeled(label: "tag", count: 3)), true)
    check("string interior nul", .text(value: "nul\0inside\0"), true)
    // Any NaN matches the NaN vector; zero keeps its sign.
    check("f64 nan", .f64(value: Double(bitPattern: 0x7ff8_0000_0000_0001)), true)
    check("f64 -0", .f64(value: -0.0), true)
    check("f64 -0", .f64(value: 0.0), false)
    check("u64 max", .u64(value: .max), true)
    check("enum infrared", .hue(value: .infrared), true)
    check("optional zero", .maybeI64(value: 0), true)
    check("optional absent", .maybeI64(value: nil), true)
    check("optional zero", .maybeI64(value: nil), false)
    // A map's entry order doesn't matter on the wire.
    check("map of strings", .counts(value: ["x": 0, "héllo": -1, "": .max]), true)
    check("blank", .blank, true)
    // A holder of a Swift-made token checks against the table by value.
    check("objects sparse", .objects(value: Holder(primary: Token(value: -1), spare: nil, many: [], byName: [:])), true)
}

func spotChecks(_ index: [String: UInt32]) {
    func named(_ name: String) -> Vector {
        guard let i = index[name] else { fail("no vector named \(name)") }
        return vector(i)
    }

    guard case let .i64(big) = named("i64 past 2^53"), big == -9_007_199_254_740_993 else {
        fail("i64 past 2^53")
    }
    _ = big
    guard case let .f32(tiny) = named("f32 min subnormal"), tiny.bitPattern == 1 else {
        fail("f32 min subnormal")
    }
    _ = tiny
    guard case let .text(astral) = named("string astral"), same(astral, "🦀 crab 😀") else {
        fail("string astral")
    }
    _ = astral

    guard case let .allScalars(m) = named("scalars minimum") else { fail("scalars minimum") }
    expect(m.i8Value == .min && m.u8Value == 0 && m.i16Value == .min && m.u16Value == 0, "scalars minimum ints")
    expect(m.i32Value == .min && m.u32Value == 0 && m.i64Value == .min && m.u64Value == 0, "scalars minimum wide ints")
    expect(m.f32Value == -.infinity && m.f64Value.isNaN, "scalars minimum floats")
    expect(m.color == .infrared && !m.flag, "scalars minimum color and flag")

    guard case let .deep(c) = named("composite canonical") else { fail("composite canonical") }
    expect(same(c.name, "héllo wörld ✓"), "composite name")
    expect(c.blob == Data([0, 1, 2, 253, 254, 255]), "composite blob")
    expect(c.someI64 == .min && c.noneI64 == nil && c.someText == "", "composite optionals")
    expect(c.names == ["a", "", "ccc"] && c.matrix == [[1, 2, 3], [], [-4]], "composite lists")
    expect(c.floats.count == 6 && c.floats[0].isNaN && c.floats[1] == .infinity, "composite floats")
    expect(c.floats[3].bitPattern == (-0.0 as Double).bitPattern && c.floats[4] == .leastNonzeroMagnitude, "composite float edges")
    expect(c.byName == ["one": 1, "two": 2, "neg": -3, "": .max], "composite byName")
    expect(c.byId.count == 3 && c.byId[-1]?.u32Value == 4_000_000_000 && c.byId[42]?.color == .red, "composite byId")
    expect(c.byColor == [.infrared: "below", .blue: "sky"], "composite byColor")
    expect(c.flags == [0: false, .max: true], "composite flags")
    expect(c.scalars == canonicalScalars, "composite scalars")
    expect(c.shape == .labeled(label: "tag", count: 3), "composite shape")
    expect(c.shapes.count == 6, "composite shapes")
    guard case .rect(let width, let height) = c.shapes[2] else { fail("composite shapes[2]") }
    expect(width.bitPattern == (-0.0 as Float).bitPattern && height == .infinity, "composite rect")
    guard case .nested(_, nil) = c.shapes[5] else { fail("composite shapes[5]") }
    guard case .nested(_, nil)? = c.maybeShape else { fail("composite maybeShape") }
    expect(c.maybeList == Data([9, 8]), "composite maybeList")
    expect(c.sparse == [true, nil, false], "composite sparse")
    expect(c.colors == [.red, .green, .blue, .infrared], "composite colors")
}

func outOfRange(_ n: UInt32) {
    do {
        _ = try Codec.vector(index: n)
        fail("vector(\(n)) returned")
    } catch let CodecError.outOfRange(message, index, count) {
        expect(index == n && count == n, "outOfRange payload (got \(index), \(count))")
        expect(message == "vector \(n) is out of range (count \(n))", "outOfRange message (got \(message))")
    } catch {
        fail("vector(\(n)) threw \(error)")
    }
    do {
        _ = try Codec.vectorName(index: n + 5)
        fail("vectorName(\(n + 5)) returned")
    } catch let error as CodecError {
        guard case let .outOfRange(_, index, count) = error else { fail("unexpected \(error)") }
        expect(error.errorCode == 1 && index == n + 5 && count == n, "vectorName outOfRange payload")
    } catch {
        fail("vectorName(\(n + 5)) threw \(error)")
    }
    expect(!Codec.checkVector(index: n, value: .blank), "checkVector past the end is false")
}

/// Hand `bytes` to `check_vector` through the raw C entry point: the Swift
/// encoder never produces a malformed buffer, but the producer must reject
/// one with a marshalling failure (-3).
func reject(_ bytes: [UInt8], _ what: String) {
    var err = codec_error()
    let ok = bytes.withUnsafeBufferPointer { codec_codec_check_vector(0, $0.baseAddress, $0.count, &err) }
    expect(!ok && err.code == -3, "\(what) is rejected with -3 (got \(err.code))")
    codec_error_clear(&err)
}

func le<T: FixedWidthInteger>(_ v: T) -> [UInt8] {
    withUnsafeBytes(of: v.littleEndian) { Array($0) }
}

func malformed() {
    let i64Tag: Int32 = 7, flagTag: Int32 = 11, textTag: Int32 = 12, hueTag: Int32 = 14, countsTag: Int32 = 19
    reject(le(i64Tag) + le(UInt32(7)), "a truncated value")
    reject(le(Int32(999)), "an unknown tag")
    reject(le(Int32(0)) + [0], "trailing bytes")
    reject(le(flagTag) + [2], "a bool of 2")
    reject(le(hueTag) + le(Int32(3)), "an undeclared enum value")
    let a = le(UInt32(1)) + Array("a".utf8)
    reject(le(countsTag) + le(UInt32(2)) + a + le(Int64(1)) + a + le(Int64(2)), "a repeated map key")
    reject(le(textTag) + le(UInt32(2)) + [0xC3, 0x28], "a string that isn't UTF-8")

    // Direct families: an undeclared enum value (which Swift's `Color` can't
    // even spell) and invalid UTF-8.
    var err = codec_error()
    _ = codec_codec_echo_color(codec_codec_Color(rawValue: 3), &err)
    expect(err.code == -3, "echo_color(3) is rejected (got \(err.code))")
    codec_error_clear(&err)
    var len = 0
    let bad: [UInt8] = [0xC3, 0x28]
    let out = bad.withUnsafeBufferPointer { codec_codec_echo_text($0.baseAddress, $0.count, &len, &err) }
    expect(out == nil && err.code == -3, "echo_text(invalid UTF-8) is rejected")
    codec_error_clear(&err)
}

func objects(_ index: [String: UInt32]) {
    guard let full = index["objects full"], case let .objects(h) = vector(full) else { fail("objects full") }
    expect(h.primary.value() == 10 && h.spare?.value() == 11, "objects full primary and spare")
    expect(h.many.map { $0.value() } == [12, 13, .min], "objects full many")
    expect(h.byName.mapValues { $0.value() } == ["a": 20, "b": 21], "objects full byName")
    // Each encoding mints fresh references, so the holder can be sent twice.
    let expected = [10, 11, 12, 13, 20, 21, Int64.min].reduce(0, &+)
    expect(Codec.sumHolder(holder: h) == expected && Codec.sumHolder(holder: h) == expected, "sumHolder twice")

    // primaryOf returns the very same native object (a new wrapper).
    let p = Codec.primaryOf(holder: h)
    expect(p.value() == 10, "primaryOf value")
    expect(Codec.samePrimary(a: h, b: Holder(primary: p, spare: nil, many: [], byName: [:])), "samePrimary identity")
    let twin = Token(value: 10)
    expect(!Codec.samePrimary(a: h, b: Holder(primary: twin, spare: nil, many: [], byName: [:])), "an equal value is not the same object")

    // A holder of Swift-made tokens, the same wrapper in several slots.
    let mine = Holder(primary: twin, spare: twin, many: [twin, twin, Token(value: -4)], byName: ["k": twin])
    expect(Codec.sumHolder(holder: mine) == 10 * 5 - 4, "sumHolder of consumer tokens")
    expect(twin.value() == 10, "the twin is still usable")
}

func run() {
    let n = Codec.vectorCount()
    expect(n >= 60, "vectorCount (got \(n))")
    let index = vectorIndex(n)
    expect(index.count == Int(n), "vector names are unique")
    everyVector(n)
    literalVectors(index)
    spotChecks(index)
    outOfRange(n)
    malformed()
    objects(index)
    print("swift/codec: \(n) vectors")
}

run()
expect(codec_abi_version() == 4, "ABI revision 4")
expect(codec_debug_live(-1) == 1, "the sample counts live resources")
assertNoLeaks()
print("swift/codec: OK")
