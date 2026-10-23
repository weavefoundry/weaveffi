// Conformance consumer: codec sample, Swift target (ABI revision 5).
//
// The shared-vector loop through the generated `Codec` module: every vector
// the producer serves is decoded by the Swift codec, re-encoded on the way
// back into `checkVector` (which must accept it, and reject it for the
// neighboring index), and, for primitive vectors, pushed through the
// matching direct-family `echo*`, optional-scalar `echoOpt*`, and
// typed-array `echo*s`. Then vectors built from Swift literals (so a
// symmetric encode/decode bug can't hide), spot checks of decoded values,
// the typed `CodecError.outOfRange` with its payload, optional scalars and
// typed arrays at their edges (NaN, -0.0, extremes, empty), `usize`, `char`,
// and a custom type crossing as strings, a `NativeSequence` of typed-array
// chunks, malformed buffers, invalid optional enums, misaligned arrays, and
// rejected conversions sent through the raw C entry points (the Swift
// wrappers can't produce them, and the conversions trap there), and object
// identity (`==` and hashing by native object) and reference counting
// through buffers. Ends by asserting the producer's leak counters are zero.
// Exits non-zero on any mismatch.

import CCodec
import Codec
import Foundation

func fail(_ msg: String, line: UInt = #line) -> Never {
    FileHandle.standardError.write(Data("assertion failed (line \(line)): \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: @autoclosure () -> String, line: UInt = #line) {
    if !cond { fail(msg(), line: line) }
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

/// Push a primitive vector's value through its direct-family echo, and,
/// where one exists, its optional-scalar and typed-array echoes.
func echo(_ v: Vector) {
    switch v {
    case let .i8(x): expect(Codec.echoI8(value: x) == x, "echoI8(\(x))")
    case let .u8(x): expect(Codec.echoU8(value: x) == x, "echoU8(\(x))")
    case let .i16(x): expect(Codec.echoI16(value: x) == x, "echoI16(\(x))")
    case let .u16(x): expect(Codec.echoU16(value: x) == x, "echoU16(\(x))")
    case let .i32(x):
        expect(Codec.echoI32(value: x) == x, "echoI32(\(x))")
        expect(Codec.echoOptI32(value: x) == x, "echoOptI32(\(x))")
        expect(Codec.echoI32s(values: [x, x]) == [x, x], "echoI32s([\(x)])")
    case let .u32(x): expect(Codec.echoU32(value: x) == x, "echoU32(\(x))")
    case let .i64(x): expect(Codec.echoI64(value: x) == x, "echoI64(\(x))")
    case let .u64(x):
        expect(Codec.echoU64(value: x) == x, "echoU64(\(x))")
        expect(Codec.echoUsize(value: x) == x, "echoUsize(\(x))")
        expect(Codec.echoU64s(values: [x]) == [x], "echoU64s([\(x)])")
    case let .f32(x):
        expect(Codec.echoF32(value: x).bitPattern == x.bitPattern, "echoF32(\(x)) bit for bit")
    case let .f64(x):
        expect(Codec.echoF64(value: x).bitPattern == x.bitPattern, "echoF64(\(x)) bit for bit")
        expect(Codec.echoOptF64(value: x).map { $0.bitPattern == x.bitPattern || ($0.isNaN && x.isNaN) } == true, "echoOptF64(\(x))")
        expect(Codec.echoF64s(values: [x]).map { $0.isNaN ? 0 : $0.bitPattern } == [x.isNaN ? 0 : x.bitPattern], "echoF64s([\(x)])")
    case let .flag(x):
        expect(Codec.echoBool(value: x) == x, "echoBool(\(x))")
        expect(Codec.echoOptBool(value: x) == x, "echoOptBool(\(x))")
    case let .text(x): expect(same(Codec.echoText(value: x), x), "echoText(\(x.debugDescription))")
    case let .blob(x): expect(Codec.echoBlob(value: x) == x, "echoBlob(\(x as NSData))")
    case let .hue(x):
        expect(Codec.echoColor(value: x) == x, "echoColor(\(x))")
        expect(Codec.echoOptColor(value: x) == x, "echoOptColor(\(x))")
    case let .maybeI64(x):
        // An `i64?` in a buffer; through the direct `i32?` echo when it fits.
        if let x = x, let small = Int32(exactly: x) {
            expect(Codec.echoOptI32(value: small) == small, "echoOptI32(\(small))")
        }
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
    _ = codec_codec_echo_color(3, &err)
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

    // primaryOf returns the very same native object (a new wrapper), which
    // compares and hashes equal to the holder's.
    let p = Codec.primaryOf(holder: h)
    expect(p.value() == 10, "primaryOf value")
    expect(p == h.primary && p.hashValue == h.primary.hashValue && Set([p, h.primary]).count == 1, "primaryOf is == the primary")
    expect(p != Token(value: 10), "an equal value is a different object")
    // Records carrying objects are Hashable too.
    let again = Holder(primary: p, spare: h.spare, many: h.many, byName: h.byName)
    expect(again == h && Set([again, h]).count == 1, "holders of the same objects are equal")
    expect(Codec.samePrimary(a: h, b: Holder(primary: p, spare: nil, many: [], byName: [:])), "samePrimary identity")
    let twin = Token(value: 10)
    expect(!Codec.samePrimary(a: h, b: Holder(primary: twin, spare: nil, many: [], byName: [:])), "an equal value is not the same object")

    // A holder of Swift-made tokens, the same wrapper in several slots.
    let mine = Holder(primary: twin, spare: twin, many: [twin, twin, Token(value: -4)], byName: ["k": twin])
    expect(Codec.sumHolder(holder: mine) == 10 * 5 - 4, "sumHolder of consumer tokens")
    expect(twin.value() == 10, "the twin is still usable")
}

func optScalars() {
    expect(Codec.echoOptI32(value: nil) == nil, "echoOptI32(nil)")
    expect(Codec.echoOptI32(value: .min) == .min, "echoOptI32(min)")
    expect(Codec.echoOptI32(value: 0) == 0, "echoOptI32(0)")
    guard let zero = Codec.echoOptF64(value: -0.0) else { fail("echoOptF64(-0.0)") }
    expect(zero == 0 && zero.sign == .minus, "echoOptF64 keeps the sign of zero")
    expect(Codec.echoOptF64(value: .nan)?.isNaN == true, "echoOptF64(nan)")
    expect(Codec.echoOptF64(value: nil) == nil, "echoOptF64(nil)")
    expect(Codec.echoOptBool(value: true) == true && Codec.echoOptBool(value: false) == false, "echoOptBool")
    expect(Codec.echoOptBool(value: nil) == nil, "echoOptBool(nil)")
    expect(Codec.echoOptColor(value: .infrared) == .infrared && Codec.echoOptColor(value: .blue) == .blue, "echoOptColor")
    expect(Codec.echoOptColor(value: nil) == nil, "echoOptColor(nil)")

    // A present raw value Swift's `Color` can't spell is rejected; an
    // absent one is ignored.
    var err = codec_error()
    var out: Int32 = 0
    expect(!codec_codec_echo_opt_color(true, 3, &out, &err) && err.code == -3, "echo_opt_color(3) is rejected (got \(err.code))")
    codec_error_clear(&err)
    expect(!codec_codec_echo_opt_color(false, 3, &out, &err) && err.code == 0, "an absent color is ignored")
}

func typedArrays() {
    let f64s = [Double(bitPattern: 0x7ff8_0000_0000_0001), -0.0, .leastNonzeroMagnitude, .infinity]
    let back = Codec.echoF64s(values: f64s)
    expect(back.count == 4 && back[0].isNaN, "echoF64s NaN")
    expect(back[1...].map(\.bitPattern) == f64s[1...].map(\.bitPattern), "echoF64s bit for bit")
    expect(Codec.echoI32s(values: [.min, 0, .max]) == [.min, 0, .max], "echoI32s extremes")
    expect(Codec.echoI32s(values: []) == [], "echoI32s([])")
    expect(Codec.echoU64s(values: [.max, 1 << 63]) == [18_446_744_073_709_551_615, 9_223_372_036_854_775_808], "echoU64s extremes")
    expect(Codec.echoU64s(values: []) == [], "echoU64s([])")
    // An array slice copied out lends its own storage.
    let wide: [UInt64] = [7, 8, 9]
    expect(Codec.echoU64s(values: Array(wide.dropFirst())) == [8, 9], "echoU64s of a slice")

    // A null array with a length, and one not aligned for its element.
    var err = codec_error()
    var len = 0
    expect(codec_codec_echo_u64s(nil, 2, &len, &err) == nil && err.code == -3, "a null array with a length is rejected")
    codec_error_clear(&err)
    let raw = UnsafeMutableRawPointer.allocate(byteCount: 24, alignment: 8)
    defer { raw.deallocate() }
    raw.initializeMemory(as: UInt8.self, repeating: 0, count: 24)
    let misaligned = UnsafePointer<UInt64>(OpaquePointer(raw + 4))
    expect(codec_codec_echo_u64s(misaligned, 2, &len, &err) == nil && err.code == -3, "a misaligned array is rejected")
    codec_error_clear(&err)
}

func chunks() {
    let extremes = Array(Codec.chunks(values: [.min, 0, .max], size: 2))
    expect(extremes == [[-2_147_483_648, 0], [2_147_483_647]], "chunks of extremes (got \(extremes))")
    let seq = Codec.chunks(values: [1, 2, 3, 4], size: 2)
    do {
        expect(try seq.collect() == [[1, 2], [3, 4]], "chunks of four")
    } catch {
        fail("collect threw \(error)")
    }
    expect(seq.next() == nil && seq.error == nil, "an exhausted sequence stays at the end")
    expect(Array(Codec.chunks(values: [1, 2], size: 0)).isEmpty, "chunks of size 0")
    expect(Array(Codec.chunks(values: [], size: 3)).isEmpty, "chunks of nothing")
    // Abandoning a sequence part-way releases the native iterator.
    do {
        let partial = Codec.chunks(values: [1, 2, 3], size: 1)
        withExtendedLifetime(partial) {
            expect(partial.next() == [1], "first chunk")
            expect(codec_debug_live(2) == 1, "one live iterator")
        }
    }
    expect(codec_debug_live(2) == 0, "the abandoned iterator was released")
}

/// Echo `input` through the raw C `f`, expecting a -3 with `message`.
func rejected(
    _ f: (UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<Int>?, UnsafeMutablePointer<codec_error>?) -> UnsafePointer<UInt8>?,
    _ input: String, _ message: String
) {
    var err = codec_error()
    var len = 0
    let bytes = Array(input.utf8)
    let out = bytes.withUnsafeBufferPointer { f($0.baseAddress, $0.count, &len, &err) }
    let got = err.message_ptr.map { String(decoding: UnsafeBufferPointer(start: $0, count: err.message_len), as: UTF8.self) } ?? ""
    expect(out == nil && err.code == -3 && got == message, "\(input.debugDescription) rejected with \(err.code) \(got.debugDescription)")
    codec_error_clear(&err)
}

func conversions() {
    expect(Codec.echoUsize(value: 4_294_967_295) == 4_294_967_295, "echoUsize(u32 max)")
    expect(Codec.echoUsize(value: .max) == .max, "echoUsize(u64 max)")

    for c in ["🦀", "é", "a"] {
        expect(same(Codec.echoChar(value: c), c), "echoChar(\(c))")
    }
    rejected(codec_codec_echo_char, "ab", "value: \"ab\" is not a valid char")
    rejected(codec_codec_echo_char, "", "value: \"\" is not a valid char")

    expect(Codec.echoHex(value: "ff") == "ff", "echoHex(ff)")
    expect(Codec.echoHex(value: "00FF") == "ff", "echoHex(00FF) normalizes")
    expect(Codec.echoHex(value: "0") == "0", "echoHex(0)")
    rejected(codec_codec_echo_hex, "xyz", "value: invalid digit found in string")
    rejected(codec_codec_echo_hex, "", "value: cannot parse integer from empty string")
    rejected(codec_codec_echo_hex, "100000000", "value: number too large to fit in target type")
}

func run() {
    do {
        try CodecLibrary.check()
    } catch {
        fail("check() threw \(error)")
    }
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
    optScalars()
    typedArrays()
    chunks()
    conversions()
    print("swift/codec: \(n) vectors")
}

run()
expect(codec_abi_version() == 5, "ABI revision 5")
expect(codec_debug_live(-1) == 1, "the sample counts live resources")
assertNoLeaks()
print("swift/codec: OK")
