// Conformance consumer: codec sample, Kotlin (JVM via JNI) target.
//
// Drives the value-buffer round-trip oracle through the generated Kotlin
// encoder and decoder (`WeaveBufferWriter`/`WeaveBufferReader` plus the
// per-type `pack*`/`unpack*` routines). For `Scalars` and `Composite` the
// consumer decodes the producer's canonical fixture and checks every field
// against concrete values, hands the same value back through `verify*` (which
// proves the Kotlin encoder produced exactly the bytes Rust encodes), and
// compares `roundtrip*` output field by field. It then builds its own values
// from scratch with edge cases (empty strings, lists, maps, and byte arrays;
// present-but-empty optionals; BMP and supplementary unicode; the i8..u64
// extremes, with unsigned values carried in Kotlin's signed types; NaN, both
// infinities, negative zero, and a subnormal) and round-trips them, exercises
// every `Shape` variant alone and inside lists and optionals, checks the
// typed `CodecException.Mismatch` from a rejected fixture, and exercises
// `Holder` (objects inside records, optionals, and lists: `sumHolder`,
// `primaryOf` returning the same native object as `holder.primary`,
// `samePrimary`, and clean release of every adopted reference with double
// `close()` safe and use after close rejected). Compiled in-module with the
// generated `WeaveFFI.kt`, so the `internal` buffer helpers and `handle` are
// reachable.
@file:JvmName("Main")

import com.weaveffi.CodecException
import com.weaveffi.Color
import com.weaveffi.Composite
import com.weaveffi.Holder
import com.weaveffi.Scalars
import com.weaveffi.Shape
import com.weaveffi.Token
import com.weaveffi.WeaveFFI
import com.weaveffi.WeaveFFIException
import com.weaveffi.packComposite
import com.weaveffi.unpackComposite
import com.weaveffi.weaveDecode
import com.weaveffi.weaveEncode
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

/** Run `block` and return the exception it threw, or null if it completed. */
inline fun thrownBy(block: () -> Unit): Throwable? =
    try {
        block()
        null
    } catch (e: Throwable) {
        e
    }

/** The canonical `Scalars` fixture, spelled with Kotlin's signed carriers for the unsigned fields. */
fun canonicalScalars(): Scalars = Scalars(
    i8_value = (-8).toByte(),
    u8_value = 200.toByte(),
    i16_value = (-16_000).toShort(),
    u16_value = 60_000.toShort(),
    i32_value = -2_000_000_000,
    u32_value = 4_000_000_000L,
    i64_value = -9_007_199_254_740_993L,
    u64_value = ULong.MAX_VALUE.toLong(),
    f32_value = 1.5f,
    f64_value = -2.25e100,
    flag = true,
    color = Color.Blue,
)

/**
 * Field-by-field equality for `Composite`: the data class holds `ByteArray`
 * fields, whose generated `equals` compares identity, so the comparison has
 * to spell them out with `contentEquals`.
 */
fun compositeEquals(a: Composite, b: Composite): Boolean =
    a.name == b.name &&
        a.blob.contentEquals(b.blob) &&
        a.some_i64 == b.some_i64 &&
        a.none_i64 == b.none_i64 &&
        a.some_text == b.some_text &&
        a.names == b.names &&
        a.matrix == b.matrix &&
        a.empty == b.empty &&
        a.by_name == b.by_name &&
        a.by_id == b.by_id &&
        a.scalars == b.scalars &&
        a.shape == b.shape &&
        a.shapes == b.shapes &&
        a.maybe_shape == b.maybe_shape &&
        ((a.maybe_list == null && b.maybe_list == null) ||
            (a.maybe_list != null && b.maybe_list != null && a.maybe_list.contentEquals(b.maybe_list))) &&
        a.sparse == b.sparse &&
        a.colors == b.colors

fun checkScalars() {
    val canonical = canonicalScalars()
    val sample = WeaveFFI.sampleScalars()
    // Producer encodes, consumer decodes: every field, with the unsigned ones
    // read back through their signed carriers.
    expect(sample.i8_value.toInt() == -8, "i8 (got ${sample.i8_value})")
    expect(sample.u8_value.toUByte().toInt() == 200, "u8 (got ${sample.u8_value.toUByte()})")
    expect(sample.i16_value.toInt() == -16_000, "i16 (got ${sample.i16_value})")
    expect(sample.u16_value.toUShort().toInt() == 60_000, "u16 (got ${sample.u16_value.toUShort()})")
    expect(sample.i32_value == -2_000_000_000, "i32 (got ${sample.i32_value})")
    expect(sample.u32_value == 4_000_000_000L, "u32 (got ${sample.u32_value})")
    expect(sample.i64_value == -9_007_199_254_740_993L, "i64 (got ${sample.i64_value})")
    expect(sample.u64_value.toULong() == ULong.MAX_VALUE, "u64 max (got ${sample.u64_value.toULong()})")
    expect(sample.f32_value == 1.5f, "f32 (got ${sample.f32_value})")
    expect(sample.f64_value == -2.25e100, "f64 (got ${sample.f64_value})")
    expect(sample.flag, "flag")
    expect(sample.color == Color.Blue, "color Blue (got ${sample.color})")
    expect(sample == canonical, "sample equals the locally built canonical Scalars")

    // Consumer encodes, producer decodes and compares with its canonical.
    expect(WeaveFFI.verifyScalars(sample), "verifyScalars(sample)")
    expect(WeaveFFI.verifyScalars(canonical), "verifyScalars(locally built canonical)")
    expect(WeaveFFI.roundtripScalars(sample) == sample, "roundtripScalars(sample) equals sample")

    // A rejected fixture is the typed domain error.
    val mismatch = thrownBy { WeaveFFI.verifyScalars(canonical.copy(flag = false)) }
    expect(mismatch is CodecException.Mismatch, "verifyScalars(modified) throws Mismatch (got $mismatch)")
    expect((mismatch as WeaveFFIException).code == 1, "Mismatch code 1 (got ${mismatch.code})")
    expect(mismatch.message == "value does not match the canonical fixture", "Mismatch message (got ${mismatch.message})")
    expect(thrownBy { WeaveFFI.verifyScalars(canonical.copy(u64_value = 0L)) } is CodecException.Mismatch, "u64 difference detected")
    expect(thrownBy { WeaveFFI.verifyScalars(canonical.copy(color = Color.Red)) } is CodecException.Mismatch, "enum difference detected")

    // From scratch, at the extremes of every width, plus the special floats.
    val edge = Scalars(
        i8_value = Byte.MIN_VALUE,
        u8_value = (-1).toByte(),
        i16_value = Short.MIN_VALUE,
        u16_value = (-1).toShort(),
        i32_value = Int.MIN_VALUE,
        u32_value = 0xFFFF_FFFFL,
        i64_value = Long.MIN_VALUE,
        u64_value = Long.MIN_VALUE,
        f32_value = Float.NaN,
        f64_value = -0.0,
        flag = false,
        color = Color.Red,
    )
    val edgeBack = WeaveFFI.roundtripScalars(edge)
    expect(edgeBack.i8_value == Byte.MIN_VALUE, "i8 min round-trips")
    expect(edgeBack.u8_value.toUByte() == UByte.MAX_VALUE, "u8 255 round-trips")
    expect(edgeBack.i16_value == Short.MIN_VALUE, "i16 min round-trips")
    expect(edgeBack.u16_value.toUShort() == UShort.MAX_VALUE, "u16 65535 round-trips")
    expect(edgeBack.i32_value == Int.MIN_VALUE, "i32 min round-trips")
    expect(edgeBack.u32_value == 0xFFFF_FFFFL, "u32 max round-trips (got ${edgeBack.u32_value})")
    expect(edgeBack.i64_value == Long.MIN_VALUE, "i64 min round-trips")
    expect(edgeBack.u64_value.toULong() == 9_223_372_036_854_775_808UL, "u64 2^63 round-trips")
    expect(edgeBack.f32_value.isNaN(), "f32 NaN round-trips")
    expect(edgeBack.f32_value.toRawBits() == Float.NaN.toRawBits(), "f32 NaN bits preserved")
    expect(edgeBack.f64_value == 0.0 && 1.0 / edgeBack.f64_value < 0.0, "f64 -0.0 round-trips as negative zero")
    expect(edgeBack.f64_value.toRawBits() == (-0.0).toRawBits(), "f64 -0.0 bits preserved")
    expect(edgeBack.color == Color.Red && !edgeBack.flag, "enum and flag round-trip")
    expect(edgeBack == edge, "edge Scalars equals (data class compares floats by bits)")
    val maxes = edge.copy(
        i8_value = Byte.MAX_VALUE,
        u8_value = 0,
        i16_value = Short.MAX_VALUE,
        u16_value = 0,
        i32_value = Int.MAX_VALUE,
        u32_value = 0L,
        i64_value = Long.MAX_VALUE,
        u64_value = 0L,
        f32_value = Float.NEGATIVE_INFINITY,
        f64_value = Double.POSITIVE_INFINITY,
        color = Color.Green,
    )
    expect(WeaveFFI.roundtripScalars(maxes) == maxes, "max Scalars round-trip")
    val subnormal = edge.copy(f32_value = Float.MIN_VALUE, f64_value = Double.MIN_VALUE)
    expect(WeaveFFI.roundtripScalars(subnormal) == subnormal, "subnormal floats round-trip")
}

fun checkComposite() {
    val canonical = canonicalScalars()
    val sample = WeaveFFI.sampleComposite()
    expect(sample.name == "héllo wörld ✓", "name (got ${sample.name})")
    expect(sample.blob.contentEquals(byteArrayOf(0, 1, 2, 253.toByte(), 254.toByte(), 255.toByte())), "blob (got ${sample.blob.toList()})")
    expect(sample.some_i64 == Long.MIN_VALUE, "some_i64 is i64::MIN (got ${sample.some_i64})")
    expect(sample.none_i64 == null, "none_i64 absent")
    expect(sample.some_text != null && sample.some_text.isEmpty(), "some_text present and empty (got ${sample.some_text})")
    expect(sample.names == listOf("a", "", "ccc"), "names (got ${sample.names})")
    expect(sample.matrix == listOf(listOf(1, 2, 3), listOf(), listOf(-4)), "matrix (got ${sample.matrix})")
    expect(sample.empty.isEmpty(), "empty list")
    expect(sample.by_name == mapOf("one" to 1L, "two" to 2L, "neg" to -3L), "by_name (got ${sample.by_name})")
    expect(sample.by_id.keys == setOf(-1, 42), "by_id keys (got ${sample.by_id.keys})")
    expect(sample.by_id[-1] == canonical, "by_id[-1] is the canonical Scalars")
    expect(sample.by_id[42] == canonical.copy(flag = false), "by_id[42] is canonical with flag=false")
    expect(sample.scalars == canonical, "scalars nested record")
    expect(sample.shape == Shape.Labeled("tag", 3), "shape (got ${sample.shape})")
    expect(
        sample.shapes == listOf(
            Shape.Empty,
            Shape.Circle(2.5),
            Shape.Rect(1.0f, 0.5f),
            Shape.Labeled("", -1),
            Shape.Nested(canonical, "n"),
        ),
        "shapes (got ${sample.shapes})"
    )
    expect(sample.maybe_shape == Shape.Nested(canonical, null), "maybe_shape (got ${sample.maybe_shape})")
    expect(sample.maybe_list != null && sample.maybe_list.contentEquals(byteArrayOf(9, 8)), "maybe_list (got ${sample.maybe_list?.toList()})")
    expect(sample.sparse == listOf(true, null, false), "sparse (got ${sample.sparse})")
    expect(sample.colors == listOf(Color.Red, Color.Green, Color.Blue), "colors (got ${sample.colors})")

    // Consumer encodes, producer decodes and compares.
    expect(WeaveFFI.verifyComposite(sample), "verifyComposite(sample)")
    val back = WeaveFFI.roundtripComposite(sample)
    expect(compositeEquals(back, sample), "roundtripComposite(sample) equals sample")
    // The local encoder and decoder agree with each other too.
    val local = weaveDecode(weaveEncode { w -> packComposite(w, sample) }) { r -> unpackComposite(r) }
    expect(compositeEquals(local, sample), "local encode/decode round trip")

    // The producer's rendering is a debugging aid: check it saw our unicode.
    val described = WeaveFFI.describeComposite(sample)
    expect(described.contains("héllo wörld ✓"), "describeComposite carries the name (got $described)")
    expect(described.contains("Labeled { label: \"tag\", count: 3 }"), "describeComposite carries the shape")

    // Any single change is detected by the producer.
    val changed = sample.copy(sparse = listOf(true, true, false))
    val mismatch = thrownBy { WeaveFFI.verifyComposite(changed) }
    expect(mismatch is CodecException.Mismatch, "verifyComposite(changed) throws Mismatch (got $mismatch)")
    expect(thrownBy { WeaveFFI.verifyComposite(sample.copy(none_i64 = 0L)) } is CodecException.Mismatch, "present vs absent optional detected")
    expect(thrownBy { WeaveFFI.verifyComposite(sample.copy(maybe_list = null)) } is CodecException.Mismatch, "absent list detected")
    expect(thrownBy { WeaveFFI.verifyComposite(sample.copy(some_text = null)) } is CodecException.Mismatch, "absent empty string detected")

    // From scratch: everything empty or absent, with supplementary unicode.
    val bare = Composite(
        name = "",
        blob = byteArrayOf(),
        some_i64 = null,
        none_i64 = null,
        some_text = null,
        names = listOf(),
        matrix = listOf(),
        empty = listOf(),
        by_name = mapOf(),
        by_id = mapOf(),
        scalars = canonical,
        shape = Shape.Empty,
        shapes = listOf(),
        maybe_shape = null,
        maybe_list = null,
        sparse = listOf(),
        colors = listOf(),
    )
    val bareBack = WeaveFFI.roundtripComposite(bare)
    expect(compositeEquals(bareBack, bare), "bare Composite round-trips (got ${WeaveFFI.describeComposite(bareBack)})")
    expect(bareBack.maybe_list == null && bareBack.some_text == null, "absent optionals stay absent")

    val full = Composite(
        name = "日本語 🚀 \u0000 emoji and NUL",
        blob = ByteArray(256) { it.toByte() },
        some_i64 = Long.MAX_VALUE,
        none_i64 = Long.MIN_VALUE,
        some_text = "🚀🚀",
        names = listOf("", "x".repeat(1000), "ünïcödé"),
        matrix = listOf(listOf(), listOf(Int.MIN_VALUE, Int.MAX_VALUE), listOf(0)),
        empty = listOf(Double.NaN, Double.NEGATIVE_INFINITY, -0.0, Double.MIN_VALUE),
        by_name = mapOf("" to 0L, "k" to Long.MIN_VALUE, "🚀" to Long.MAX_VALUE),
        by_id = mapOf(Int.MIN_VALUE to canonical, Int.MAX_VALUE to canonical.copy(color = Color.Red), 0 to canonical),
        scalars = canonical.copy(f32_value = Float.POSITIVE_INFINITY),
        shape = Shape.Nested(canonical, ""),
        shapes = listOf(Shape.Circle(Double.NaN), Shape.Rect(-0.0f, Float.MIN_VALUE), Shape.Labeled("🚀", Int.MIN_VALUE), Shape.Empty, Shape.Empty),
        maybe_shape = Shape.Circle(-0.0),
        maybe_list = byteArrayOf(),
        sparse = listOf(null, null, true),
        colors = listOf(Color.Blue, Color.Blue, Color.Red),
    )
    val fullBack = WeaveFFI.roundtripComposite(full)
    expect(compositeEquals(fullBack, full), "full Composite round-trips (got ${WeaveFFI.describeComposite(fullBack)})")
    expect(fullBack.maybe_list != null && fullBack.maybe_list.isEmpty(), "present-but-empty bytes stay present")
    expect(fullBack.empty[0].isNaN() && fullBack.empty[2].toRawBits() == (-0.0).toRawBits(), "special doubles inside a list")
    expect((fullBack.shapes[0] as Shape.Circle).radius.isNaN(), "NaN inside a rich enum")
    expect((fullBack.shapes[1] as Shape.Rect).width.toRawBits() == (-0.0f).toRawBits(), "-0.0f inside a rich enum")
    expect(fullBack.name == full.name, "supplementary characters and NUL survive inside a buffer")
    expect(WeaveFFI.describeComposite(full).contains("日本語 🚀 \\0 emoji and NUL"), "producer decoded the unicode name")
}

fun checkShapes() {
    val canonical = canonicalScalars()
    val all = listOf(
        Shape.Empty,
        Shape.Circle(2.5),
        Shape.Rect(1.0f, 0.5f),
        Shape.Labeled("tag", 3),
        Shape.Nested(canonical, "n"),
        Shape.Nested(canonical.copy(flag = false), null),
    )
    for (s in all) {
        expect(WeaveFFI.roundtripShape(s) == s, "roundtripShape($s)")
    }
    expect(WeaveFFI.roundtripShapes(all) == all, "roundtripShapes(all)")
    expect(WeaveFFI.roundtripShapes(listOf()).isEmpty(), "roundtripShapes(empty)")
    expect(WeaveFFI.roundtripShape(Shape.Empty) === Shape.Empty, "Empty decodes to the singleton")
    expect(WeaveFFI.describeShape(Shape.Empty) == "Empty", "describe Empty")
    expect(WeaveFFI.describeShape(Shape.Circle(2.5)) == "Circle { radius: 2.5 }", "describe Circle (got ${WeaveFFI.describeShape(Shape.Circle(2.5))})")
    expect(WeaveFFI.describeShape(Shape.Rect(1.0f, 0.5f)) == "Rect { width: 1.0, height: 0.5 }", "describe Rect")
    expect(WeaveFFI.describeShape(Shape.Labeled("tag", 3)) == "Labeled { label: \"tag\", count: 3 }", "describe Labeled")
    val nested = WeaveFFI.describeShape(Shape.Nested(canonical, null))
    expect(nested.startsWith("Nested { inner: Scalars { i8_value: -8, u8_value: 200,") && nested.endsWith("note: None }"), "describe Nested (got $nested)")
    expect(WeaveFFI.describeShape(Shape.Labeled("🚀", -1)) == "Labeled { label: \"🚀\", count: -1 }", "describe Labeled with emoji")
}

fun checkPrimitives() {
    expect(WeaveFFI.roundtripOptI64(null) == null, "roundtripOptI64(null)")
    expect(WeaveFFI.roundtripOptI64(Long.MIN_VALUE) == Long.MIN_VALUE, "roundtripOptI64(min)")
    expect(WeaveFFI.roundtripOptI64(0L) == 0L, "roundtripOptI64(0)")
    expect(WeaveFFI.roundtripMap(mapOf()) == mapOf<String, Long>(), "roundtripMap(empty)")
    val m = mapOf("" to 0L, "a" to -1L, "héllo 🚀" to Long.MAX_VALUE)
    expect(WeaveFFI.roundtripMap(m) == m, "roundtripMap (got ${WeaveFFI.roundtripMap(m)})")

    // Direct strings: BMP and supplementary characters both survive the JNI
    // crossing (the bridge converts to standard UTF-8, not modified UTF-8).
    for (s in listOf("", "plain", "héllo wörld ✓", "rocket 🚀 end", "🚀", "\uFFFF")) {
        expect(WeaveFFI.roundtripString(s) == s, "roundtripString(${s.toByteArray().toList()})")
    }
    // A C string can't carry U+0000: the producer reports a marshalling error
    // rather than truncating.
    val nul = thrownBy { WeaveFFI.roundtripString("a\u0000b") }
    expect(nul is WeaveFFIException && nul.code == -3, "embedded NUL is rejected with code -3 (got $nul)")
    expect(WeaveFFI.roundtripBytes(byteArrayOf()).isEmpty(), "roundtripBytes(empty)")
    val allBytes = ByteArray(256) { it.toByte() }
    expect(WeaveFFI.roundtripBytes(allBytes).contentEquals(allBytes), "roundtripBytes(0..255)")

    expect(WeaveFFI.roundtripI64(Long.MIN_VALUE) == Long.MIN_VALUE, "roundtripI64(min)")
    expect(WeaveFFI.roundtripI64(Long.MAX_VALUE) == Long.MAX_VALUE, "roundtripI64(max)")
    expect(WeaveFFI.roundtripI64(-1L) == -1L, "roundtripI64(-1)")
    expect(WeaveFFI.roundtripU64(ULong.MAX_VALUE.toLong()).toULong() == ULong.MAX_VALUE, "roundtripU64(max)")
    expect(WeaveFFI.roundtripU64(Long.MIN_VALUE).toULong() == 9_223_372_036_854_775_808UL, "roundtripU64(2^63)")
    expect(WeaveFFI.roundtripU64(0L) == 0L, "roundtripU64(0)")
    expect(WeaveFFI.roundtripF64(Double.NaN).isNaN(), "roundtripF64(NaN)")
    expect(WeaveFFI.roundtripF64(Double.POSITIVE_INFINITY) == Double.POSITIVE_INFINITY, "roundtripF64(+inf)")
    expect(WeaveFFI.roundtripF64(Double.NEGATIVE_INFINITY) == Double.NEGATIVE_INFINITY, "roundtripF64(-inf)")
    expect(WeaveFFI.roundtripF64(-0.0).toRawBits() == (-0.0).toRawBits(), "roundtripF64(-0.0) keeps the sign")
    expect(WeaveFFI.roundtripF64(Double.MIN_VALUE) == Double.MIN_VALUE, "roundtripF64(subnormal)")
    expect(WeaveFFI.roundtripF64(Double.MAX_VALUE) == Double.MAX_VALUE, "roundtripF64(max)")
    expect(WeaveFFI.roundtripBool(true) && !WeaveFFI.roundtripBool(false), "roundtripBool")
    expect(WeaveFFI.roundtripColor(Color.Blue) == Color.Blue, "roundtripColor(Blue)")
    expect(WeaveFFI.roundtripColor(Color.Red) == Color.Red, "roundtripColor(Red)")
    expect(Color.Blue.value == 7 && Color.fromValue(7) == Color.Blue, "Color discriminants")
}

fun checkHolders() {
    // Objects inside a record decoded from a buffer: each token is one adopted
    // strong reference wrapped in a Token.
    val holder = WeaveFFI.makeHolder(10L, true)
    expect(holder.primary.value() == 10L, "primary value (got ${holder.primary.value()})")
    expect(holder.spare != null && holder.spare.value() == 11L, "spare value (got ${holder.spare?.value()})")
    expect(holder.many.map { it.value() } == listOf(12L, 13L, 14L), "many values (got ${holder.many.map { it.value() }})")
    expect(holder.many.map { it.handle }.toSet().size == 3, "many are distinct objects")

    // Encoding a holder mints one fresh reference per token, so the wrappers
    // stay valid after the producer consumed the buffer.
    expect(WeaveFFI.sumHolder(holder) == 10L + 11L + 12L + 13L + 14L, "sumHolder (got ${WeaveFFI.sumHolder(holder)})")
    expect(WeaveFFI.sumHolder(holder) == 60L, "sumHolder again (wrappers still alive)")
    expect(holder.primary.value() == 10L, "primary still usable after encoding")

    // An object return is the same native object as the record's field.
    val primary = WeaveFFI.primaryOf(holder)
    expect(primary !== holder.primary, "primaryOf returns a new wrapper")
    expect(primary.handle == holder.primary.handle, "primaryOf wraps the same native object")
    expect(primary.value() == 10L, "primaryOf value")
    expect(WeaveFFI.samePrimary(holder, holder), "samePrimary(holder, holder)")
    val other = WeaveFFI.makeHolder(10L, false)
    expect(other.spare == null, "spare absent when not requested")
    expect(other.primary.value() == 10L, "other primary value equal but ...")
    expect(!WeaveFFI.samePrimary(holder, other), "... samePrimary distinguishes distinct objects")
    expect(WeaveFFI.sumHolder(other) == 10L + 12L + 13L + 14L, "sumHolder without spare")

    // A holder assembled in Kotlin from consumer-created tokens, sharing one
    // token across positions.
    val shared = Token(100L)
    val mine = Holder(primary = shared, spare = shared, many = listOf(shared, Token(1L), primary))
    expect(WeaveFFI.sumHolder(mine) == 100L + 100L + 100L + 1L + 10L, "sumHolder over a Kotlin-built holder")
    expect(WeaveFFI.samePrimary(mine, Holder(shared, null, listOf())), "samePrimary across Kotlin-built holders")
    expect(WeaveFFI.samePrimary(Holder(primary, null, listOf()), holder), "samePrimary through primaryOf's wrapper")
    val mineBack = WeaveFFI.primaryOf(mine)
    expect(mineBack.handle == shared.handle && mineBack.value() == 100L, "primaryOf a Kotlin-built holder")

    // Release everything. Closing one wrapper over a shared object leaves the
    // others valid; double close is safe; use after close throws; closing a
    // token that another wrapper still references never frees the object.
    primary.close()
    primary.close()
    expect(thrownBy { primary.value() } is IllegalStateException, "closed token rejects use")
    expect(holder.primary.value() == 10L, "the record's wrapper survives closing primaryOf's wrapper")
    expect(thrownBy { WeaveFFI.sumHolder(mine) } is IllegalStateException, "encoding a holder with a closed token throws")
    mineBack.close()
    expect(shared.value() == 100L, "shared token alive after closing its second wrapper")
    shared.close()
    mine.many[1].close()
    holder.primary.close()
    holder.spare!!.close()
    holder.many.forEach { it.close() }
    holder.many.forEach { it.close() }
    other.primary.close()
    other.many.forEach { it.close() }
    expect(thrownBy { WeaveFFI.sumHolder(holder) } is IllegalStateException, "fully closed holder rejects encoding")

    // Wrappers that are never closed are released by the Cleaner; create a
    // batch and let them go.
    repeat(50) { WeaveFFI.makeHolder(it.toLong(), true) }
    System.gc()
}

fun main() {
    checkScalars()
    checkComposite()
    checkShapes()
    checkPrimitives()
    checkHolders()
    println("kotlin/codec: OK")
}
