// Conformance consumer: codec sample, Kotlin (JVM via JNI) target.
//
// The shared-vector loop: for every vector the producer serves, decode it
// with the generated codec, re-encode it (which must reproduce the
// producer's bytes exactly, objects aside), pass it back to `checkVector`,
// and push each primitive vector's value through the matching direct-family
// `echo*` (unsigned values as `UByte`/`UShort`/`UInt`/`ULong`). Then vectors
// built from literals (so a symmetric encode/decode bug can't hide), spot
// checks of decoded fields, the typed `CodecException.OutOfRange` and its
// payload, malformed input rejected as marshalling failures (through the
// internal JNI bridge, which takes raw bytes), and object identity and
// reference counting through buffers. Ends by asserting the producer's leak
// counters are zero. Also the ABI 5 shapes: optional scalars, typed arrays
// (in, out, and as iterator items), `usize`, `char`, a custom type, and
// content equality of records holding bytes. Compiled with `-Xfriend-paths`, so the bindings'
// `internal` codec and bridge are reachable.
@file:JvmName("Main")

import codec.BufferWriter
import codec.CodecException
import codec.Codec
import codec.Color
import codec.Composite
import codec.Holder
import codec.JniBridge
import codec.NativeBugException
import codec.Scalars
import codec.Shape
import codec.Token
import codec.Vector
import codec.decodeBuffer
import codec.encodeBuffer
import codec.pack_Vector
import codec.unpack_Vector

/** Release the tokens a decoded vector carries (only `Objects` has any). */
fun release(v: Vector) {
    if (v is Vector.Objects) {
        val h = v.value
        h.primary.close()
        h.spare?.close()
        h.many.forEach { it.close() }
        h.byName.values.forEach { it.close() }
    }
}

fun encode(v: Vector): ByteArray = encodeBuffer { pack_Vector(it, v) }

/** The index of the vector named [name]. */
fun find(n: UInt, name: String): UInt {
    for (i in 0u until n) {
        if (Codec.vectorName(i) == name) return i
    }
    expect(false, "no vector named $name")
    return 0u
}

/**
 * Push a primitive vector's value through its direct-family echo, and the
 * ones that also cross as an optional scalar or a typed array through those.
 */
fun echo(v: Vector) {
    when (v) {
        is Vector.I32 -> {
            expect(Codec.echoI32(v.value) == v.value, "echoI32 ${v.value}")
            expect(Codec.echoOptI32(v.value) == v.value, "echoOptI32 ${v.value}")
            expect(Codec.echoI32s(listOf(v.value, v.value)) == listOf(v.value, v.value), "echoI32s ${v.value}")
        }
        is Vector.U64 -> {
            expect(Codec.echoU64(v.value) == v.value, "echoU64 ${v.value}")
            expect(Codec.echoU64s(listOf(v.value)) == listOf(v.value), "echoU64s ${v.value}")
            expect(Codec.echoUsize(v.value) == v.value, "echoUsize ${v.value}")
        }
        is Vector.F64 -> {
            val bits = v.value.toRawBits()
            expect(Codec.echoF64(v.value).toRawBits() == bits, "echoF64 ${v.value}")
            expect(Codec.echoOptF64(v.value)?.toRawBits() == bits, "echoOptF64 ${v.value}")
            expect(Codec.echoF64s(listOf(v.value)).single().toRawBits() == bits, "echoF64s ${v.value}")
        }
        is Vector.Flag -> {
            expect(Codec.echoBool(v.value) == v.value, "echoBool ${v.value}")
            expect(Codec.echoOptBool(v.value) == v.value, "echoOptBool ${v.value}")
        }
        is Vector.Hue -> {
            expect(Codec.echoColor(v.value) == v.value, "echoColor ${v.value}")
            expect(Codec.echoOptColor(v.value) == v.value, "echoOptColor ${v.value}")
        }
        is Vector.I8 -> expect(Codec.echoI8(v.value) == v.value, "echoI8 ${v.value}")
        is Vector.U8 -> expect(Codec.echoU8(v.value) == v.value, "echoU8 ${v.value}")
        is Vector.I16 -> expect(Codec.echoI16(v.value) == v.value, "echoI16 ${v.value}")
        is Vector.U16 -> expect(Codec.echoU16(v.value) == v.value, "echoU16 ${v.value}")
        is Vector.U32 -> expect(Codec.echoU32(v.value) == v.value, "echoU32 ${v.value}")
        is Vector.I64 -> expect(Codec.echoI64(v.value) == v.value, "echoI64 ${v.value}")
        is Vector.F32 -> expect(
            Codec.echoF32(v.value).toRawBits() == v.value.toRawBits(),
            "echoF32 ${v.value}",
        )
        is Vector.Text -> expect(Codec.echoText(v.value) == v.value, "echoText")
        is Vector.Blob -> expect(Codec.echoBlob(v.value).contentEquals(v.value), "echoBlob")
        else -> {}
    }
}

fun everyVector(n: UInt) {
    for (i in 0u until n) {
        val raw = JniBridge.codec_vector(i.toInt())
        val v = decodeBuffer(raw) { unpack_Vector(it) }
        if (!Codec.checkVector(i, v)) {
            expect(
                false,
                "vector $i (${Codec.vectorName(i)}) did not round-trip; producer saw ${Codec.describeVector(v)}",
            )
        }
        expect(!Codec.checkVector((i + 1u) % n, v), "vector $i never matches its neighbor")
        // The generated encoder is deterministic, so re-encoding reproduces
        // the producer's bytes, except for object tokens (fresh references).
        if (v !is Vector.Objects) {
            expect(encode(v).contentEquals(raw), "vector $i re-encodes to the producer's bytes")
        }
        echo(v)
        release(v)
    }
}

fun canonicalScalars() = Scalars(
    i8Value = -8,
    u8Value = 200u,
    i16Value = -16000,
    u16Value = 60000u,
    i32Value = -2000000000,
    u32Value = 4000000000u,
    i64Value = -9007199254740993L,
    u64Value = ULong.MAX_VALUE,
    f32Value = 1.5f,
    f64Value = -2.25e100,
    flag = true,
    color = Color.Blue,
)

fun literalVectors(n: UInt) {
    val canonical = canonicalScalars()
    expect(Codec.checkVector(find(n, "scalars canonical"), Vector.AllScalars(canonical)), "scalars canonical")
    expect(
        !Codec.checkVector(find(n, "scalars canonical"), Vector.AllScalars(canonical.copy(u16Value = 60001u))),
        "a changed field no longer matches",
    )
    expect(Codec.checkVector(find(n, "shape labeled"), Vector.Figure(Shape.Labeled("tag", 3))), "shape labeled")
    expect(
        Codec.checkVector(find(n, "string interior nul"), Vector.Text("nul\u0000inside\u0000")),
        "string interior nul",
    )
    // Any NaN matches the NaN vector; zero keeps its sign.
    expect(
        Codec.checkVector(find(n, "f64 nan"), Vector.F64(Double.fromBits(0x7ff8000000000001L))),
        "a NaN payload matches f64 nan",
    )
    expect(Codec.checkVector(find(n, "f64 -0"), Vector.F64(-0.0)), "-0.0 matches f64 -0")
    expect(!Codec.checkVector(find(n, "f64 -0"), Vector.F64(0.0)), "+0.0 doesn't match f64 -0")
    expect(Codec.checkVector(find(n, "u64 max"), Vector.U64(ULong.MAX_VALUE)), "u64 max")
    expect(Codec.checkVector(find(n, "enum infrared"), Vector.Hue(Color.Infrared)), "enum infrared")
    expect(Codec.checkVector(find(n, "optional zero"), Vector.MaybeI64(0L)), "optional zero")
    expect(Codec.checkVector(find(n, "optional absent"), Vector.MaybeI64(null)), "optional absent")
    expect(!Codec.checkVector(find(n, "optional zero"), Vector.MaybeI64(null)), "absent isn't zero")
    // A map's entry order doesn't matter on the wire.
    val counts = linkedMapOf("x" to 0L, "héllo" to -1L, "" to Long.MAX_VALUE)
    expect(Codec.checkVector(find(n, "map of strings"), Vector.Counts(counts)), "map of strings")
    expect(Codec.checkVector(find(n, "blank"), Vector.Blank), "blank")
    // A holder of a consumer-made token.
    Token(-1).use { lone ->
        val sparse = Vector.Objects(Holder(lone, null, emptyList(), emptyMap()))
        expect(Codec.checkVector(find(n, "objects sparse"), sparse), "objects sparse")
    }
}

fun fetch(i: UInt): Vector = Codec.vector(i)

fun spotChecks(n: UInt) {
    val past53 = fetch(find(n, "i64 past 2^53"))
    expect(past53 == Vector.I64(-9007199254740993L), "i64 past 2^53 is exact (got $past53)")

    val subnormal = fetch(find(n, "f32 min subnormal"))
    expect(subnormal is Vector.F32 && subnormal.value.toRawBits() == 1, "f32 min subnormal has bits 1")

    expect(fetch(find(n, "string astral")) == Vector.Text("🦀 crab 😀"), "string astral")

    val minimum = fetch(find(n, "scalars minimum"))
    expect(minimum is Vector.AllScalars, "scalars minimum is AllScalars")
    val m = (minimum as Vector.AllScalars).value
    expect(m.i8Value == Byte.MIN_VALUE && m.i16Value == Short.MIN_VALUE, "scalars minimum i8/i16")
    expect(m.i32Value == Int.MIN_VALUE && m.i64Value == Long.MIN_VALUE, "scalars minimum i32/i64")
    expect(m.u8Value == UByte.MIN_VALUE && m.u64Value == 0uL, "scalars minimum unsigned")
    expect(m.f32Value == Float.NEGATIVE_INFINITY && m.f64Value.isNaN(), "scalars minimum floats")
    expect(m.color == Color.Infrared && !m.flag, "scalars minimum color and flag")

    val deep = fetch(find(n, "composite canonical"))
    expect(deep is Vector.Deep, "composite canonical is Deep")
    val c: Composite = (deep as Vector.Deep).value
    expect(c.name == "héllo wörld ✓", "composite name (got ${c.name})")
    expect(c.blob.size == 6 && c.blob[5] == 255.toByte(), "composite blob")
    expect(c.someI64 == Long.MIN_VALUE && c.noneI64 == null, "composite optionals")
    expect(c.someText == "", "composite someText is present and empty")
    expect(c.names.size == 3 && c.names[1] == "", "composite names")
    expect(c.matrix.size == 3 && c.matrix[1].isEmpty() && c.matrix[2][0] == -4, "composite matrix")
    expect(
        c.floats.size == 6 && c.floats[0].isNaN() && c.floats[3].toRawBits() < 0,
        "composite floats",
    )
    expect(
        c.byName.size == 4 && c.byId.size == 3 && c.byColor.size == 2 && c.flags.size == 2,
        "composite maps",
    )
    expect(c.scalars.u32Value == 4000000000u, "composite scalars.u32Value")
    expect(c.shape is Shape.Labeled && (c.shape as Shape.Labeled).count == 3, "composite shape")
    val lastShape = c.shapes.last()
    expect(c.shapes.size == 6 && lastShape is Shape.Nested && lastShape.note == null, "composite shapes")
    expect(c.maybeShape is Shape.Nested, "composite maybeShape")
    expect(c.maybeList?.size == 2, "composite maybeList")
    expect(c.sparse.size == 3 && c.sparse[0] == true && c.sparse[1] == null, "composite sparse")
    expect(c.colors.size == 4 && c.colors[3] == Color.Infrared, "composite colors")
}

fun outOfRange(n: UInt) {
    val e = thrownBy { Codec.vector(n) }
    expect(e is CodecException.OutOfRange, "vector(n) raises OutOfRange (got $e)")
    val oor = e as CodecException.OutOfRange
    expect(oor.code == 1 && oor.index == n && oor.count == n, "OutOfRange payload (index ${oor.index}, count ${oor.count})")
    expect(oor.message == "vector $n is out of range (count $n)", "OutOfRange message (got ${oor.message})")

    val e2 = thrownBy { Codec.vectorName(n + 5u) }
    expect(e2 is CodecException.OutOfRange, "vectorName(n + 5) raises OutOfRange (got $e2)")
    e2 as CodecException.OutOfRange
    expect(e2.index == n + 5u && e2.count == n, "vectorName OutOfRange payload")

    expect(!Codec.checkVector(n, Vector.Blank), "checkVector past the end is false")
}

/** A raw buffer that `check_vector` must reject as a marshalling failure. */
internal fun reject(what: String, write: (BufferWriter) -> Unit) {
    val bytes = encodeBuffer(write)
    val e = thrownBy { JniBridge.codec_check_vector(0, bytes) }
    expect(e is NativeBugException && e.code == -3, "$what is rejected with -3 (got $e)")
}

fun malformed() {
    val tagI64 = 7
    reject("a truncated buffer") { it.writeI32(tagI64); it.writeU32(7u) }
    reject("an unknown tag") { it.writeI32(999) }
    reject("trailing bytes") { it.writeI32(0); it.writeU8(0u) }
    reject("a bool that is neither 0 nor 1") { it.writeI32(11); it.writeU8(2u) }
    reject("an undeclared enum value") { it.writeI32(14); it.writeI32(3) }
    reject("a repeated map key") {
        it.writeI32(19)
        it.writeU32(2u)
        it.writeString("a")
        it.writeI64(1)
        it.writeString("a")
        it.writeI64(2)
    }
    reject("a string that isn't UTF-8") { it.writeI32(12); it.writeBytes(byteArrayOf(0xC3.toByte(), 0x28)) }
    // The tags above are the declaration order of the Vector variants.
    expect(encode(Vector.I64(0)).copyOfRange(0, 4).contentEquals(byteArrayOf(7, 0, 0, 0)), "I64 tag")
    expect(encode(Vector.Counts(emptyMap()))[0] == 19.toByte(), "Counts tag")

    // The generated decoder rejects the same inputs.
    val unknown = thrownBy { decodeBuffer(byteArrayOf(0xE7.toByte(), 0x03, 0, 0)) { unpack_Vector(it) } }
    expect(unknown is NativeBugException && unknown.code == -3, "an unknown tag fails to decode")

    // Color can't spell an undeclared value, but the raw bridge can: the
    // producer rejects it with -3, a trap for a call that can't fail.
    val badColor = thrownBy { JniBridge.codec_echo_color(3) }
    expect(badColor is NativeBugException && badColor.code == -3, "an undeclared Color is rejected (got $badColor)")
    val badText = thrownBy { JniBridge.codec_echo_text(byteArrayOf(0xC3.toByte(), 0x28)) }
    expect(badText is NativeBugException && badText.code == -3, "invalid UTF-8 is rejected (got $badText)")
}

fun objects(n: UInt) {
    val full = fetch(find(n, "objects full"))
    expect(full is Vector.Objects, "objects full is Objects")
    val h = (full as Vector.Objects).value
    expect(h.primary.value() == 10L && h.spare?.value() == 11L, "holder primary and spare")
    expect(h.many.map { it.value() } == listOf(12L, 13L, Long.MIN_VALUE), "holder many")
    expect(h.byName.mapValues { it.value.value() } == mapOf("a" to 20L, "b" to 21L), "holder byName")
    // Each encoding mints fresh references, so the holder can be sent twice.
    val expected = 10L + 11 + 12 + 13 + 20 + 21 + Long.MIN_VALUE
    expect(Codec.sumHolder(h) == expected && Codec.sumHolder(h) == expected, "sumHolder twice")

    // primaryOf returns the very same object (a new reference to it).
    val p = Codec.primaryOf(h)
    expect(p.value() == 10L, "primaryOf value")
    expect(Codec.samePrimary(h, Holder(p, null, emptyList(), emptyMap())), "primaryOf is the same object")
    val twin = Token(10)
    expect(!Codec.samePrimary(h, Holder(twin, null, emptyList(), emptyMap())), "an equal value isn't the same object")

    // A holder built from consumer tokens, the same wrapper in several slots.
    val minus4 = Token(-4)
    val mine = Holder(twin, twin, listOf(twin, twin, minus4), mapOf("k" to twin))
    expect(Codec.sumHolder(mine) == 10L * 5 - 4, "consumer holder sums to 46")

    // A zero token is a marshalling failure, not a crash.
    val zero = thrownBy { JniBridge.codec_sum_holder(ByteArray(17)) }
    expect(zero is NativeBugException && zero.code == -3, "a zero token is rejected (got $zero)")

    minus4.close()
    twin.close()
    twin.close() // closing twice is safe
    p.close()
    release(full)
    expect(thrownBy { p.value() } is IllegalStateException, "a closed wrapper can't be used")
}

/** The ABI 5 shapes: optional scalars, typed arrays, usize, char, a custom type, and an iterator of arrays. */
fun abi5Shapes() {
    // Optional scalars cross as a presence flag and a value.
    expect(Codec.echoOptI32(null) == null, "echoOptI32(null)")
    expect(Codec.echoOptI32(Int.MIN_VALUE) == Int.MIN_VALUE, "echoOptI32(MIN)")
    expect(Codec.echoOptI32(0) == 0, "echoOptI32(0) is present")
    val negZero = Codec.echoOptF64(-0.0)
    expect(negZero != null && negZero.toRawBits() == (-0.0).toRawBits(), "echoOptF64(-0.0) keeps the sign")
    expect(Codec.echoOptF64(Double.NaN)?.isNaN() == true, "echoOptF64(NaN)")
    expect(Codec.echoOptF64(null) == null, "echoOptF64(null)")
    expect(Codec.echoOptBool(true) == true && Codec.echoOptBool(false) == false, "echoOptBool")
    expect(Codec.echoOptBool(null) == null, "echoOptBool(null)")
    expect(Codec.echoOptColor(Color.Infrared) == Color.Infrared, "echoOptColor(Infrared)")
    expect(Codec.echoOptColor(Color.Blue) == Color.Blue, "echoOptColor(Blue)")
    expect(Codec.echoOptColor(null) == null, "echoOptColor(null)")
    // The raw bridge can send an undeclared enum value: -3.
    val badOpt = thrownBy { JniBridge.codec_echo_opt_color(true, 3) }
    expect(badOpt is NativeBugException && badOpt.code == -3, "an undeclared Color? is rejected (got $badOpt)")
    expect(JniBridge.codec_echo_opt_color(false, 3) == null, "an absent value is ignored")

    // Typed arrays.
    val floats = listOf(Double.NaN, -0.0, Double.MIN_VALUE, Double.POSITIVE_INFINITY)
    val echoed = Codec.echoF64s(floats)
    expect(
        echoed.size == 4 && echoed.zip(floats).all { (a, b) -> a.toRawBits() == b.toRawBits() },
        "echoF64s is bit-identical (got $echoed)",
    )
    expect(Codec.echoF64s(emptyList()).isEmpty(), "echoF64s([])")
    val ints = listOf(Int.MIN_VALUE, 0, Int.MAX_VALUE)
    expect(Codec.echoI32s(ints) == ints, "echoI32s")
    expect(Codec.echoI32s(emptyList()).isEmpty(), "echoI32s([])")
    val longs = List(5000) { it * 7 }
    expect(Codec.echoI32s(longs) == longs, "echoI32s of a heap-copied array")
    val big = listOf(ULong.MAX_VALUE, 9223372036854775808uL)
    expect(Codec.echoU64s(big) == big, "echoU64s keeps unsigned values")
    expect(Codec.echoU64s(emptyList()).isEmpty(), "echoU64s([])")

    // usize crosses as u64.
    expect(Codec.echoUsize(4294967295uL) == 4294967295uL, "echoUsize(u32::MAX)")
    expect(Codec.echoUsize(ULong.MAX_VALUE) == ULong.MAX_VALUE, "echoUsize(u64::MAX)")

    // char crosses as a one-scalar string.
    for (c in listOf("\uD83E\uDD80", "é", "a")) {
        expect(Codec.echoChar(c) == c, "echoChar($c)")
    }
    val two = thrownBy { Codec.echoChar("ab") }
    expect(
        two is NativeBugException && two.code == -3 && two.message!!.endsWith("value: \"ab\" is not a valid char"),
        "echoChar(ab) is a marshalling error (got ${two?.message})",
    )
    val none = thrownBy { Codec.echoChar("") }
    expect(none is NativeBugException && none.code == -3, "echoChar(\"\") is a marshalling error (got $none)")

    // A custom type (a u32 in hex) crosses as its string repr.
    expect(Codec.echoHex("ff") == "ff" && Codec.echoHex("00FF") == "ff" && Codec.echoHex("0") == "0", "echoHex normalizes")
    for ((input, message) in listOf(
        "xyz" to "value: invalid digit found in string",
        "" to "value: cannot parse integer from empty string",
        "100000000" to "value: number too large to fit in target type",
    )) {
        val e = thrownBy { Codec.echoHex(input) }
        expect(
            e is NativeBugException && e.code == -3 && e.message!!.endsWith(message),
            "echoHex($input) is a marshalling error (got ${e?.message})",
        )
    }

    // An iterator of typed arrays.
    expect(
        Codec.chunks(listOf(Int.MIN_VALUE, 0, Int.MAX_VALUE), 2u).asSequence().toList() ==
            listOf(listOf(Int.MIN_VALUE, 0), listOf(Int.MAX_VALUE)),
        "chunks of 2",
    )
    expect(
        Codec.chunks(listOf(1, 2, 3, 4), 2u).asSequence().toList() == listOf(listOf(1, 2), listOf(3, 4)),
        "chunks([1, 2, 3, 4], 2)",
    )
    expect(!Codec.chunks(listOf(1, 2), 0u).hasNext(), "chunks of 0 is empty")
    expect(!Codec.chunks(emptyList(), 3u).hasNext(), "chunks of [] is empty")
    expect(JniBridge.debug_live(2) == 0L, "exhausted chunk iterators are released")

    // Records holding bytes compare by content.
    val blob = byteArrayOf(1, 2, 3)
    expect(Vector.Blob(blob) == Vector.Blob(blob.copyOf()), "Blob compares by content")
    expect(Vector.Blob(blob).hashCode() == Vector.Blob(blob.copyOf()).hashCode(), "Blob hashes by content")
    expect(Vector.Blob(blob) != Vector.Blob(byteArrayOf(1, 2)), "different bytes differ")
    expect(Vector.Blob(blob).toString() == "Blob(value=[1, 2, 3])", "Blob prints its bytes")
}

fun main() {
    expect(JniBridge.debug_live(-1) == 1L, "the sample counts live allocations")
    val n = Codec.vectorCount()
    expect(n >= 60u, "at least 60 vectors (got $n)")

    everyVector(n)
    literalVectors(n)
    spotChecks(n)
    outOfRange(n)
    malformed()
    objects(n)
    abi5Shapes()

    expectNoLeaks(JniBridge::debug_live)
    println("kotlin/codec: OK ($n vectors)")
}
