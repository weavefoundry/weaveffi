// Conformance consumer: shapes sample, Android/Kotlin (JNI) target.
//
// Exercises the rich (algebraic) enum surface as value types: `Shape` is a
// generated sealed class (`Empty` object, `Circle`/`Rectangle`/`Labeled` data
// classes) that crosses JNI as a value buffer packed on the Kotlin side, so
// there are no native handles and nothing to close. Drives every variant
// through `describe` (buffered rich enum in, string out) and `scale` (buffered
// in and out, decoded back into the sealed hierarchy), covering unit
// (`Empty`), f64 (`Circle`), two f32 (`Rectangle`), and string + u8
// (`Labeled`) payloads, plus the C-style `Channel` enum's plain discriminants
// and the expanded numerics (`sumBytes`: bytes in, u64 out). Mirrors the C
// consumer's assertions. Compiled in-module with the generated `WeaveFFI.kt`.
@file:JvmName("Main")

import com.weaveffi.Channel
import com.weaveffi.Shape
import com.weaveffi.WeaveFFI
import kotlin.math.abs
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

fun main() {
    // describe dispatches on the active variant: each call packs the sealed
    // value into a buffer, and the producer formats it.
    expect(WeaveFFI.describe(Shape.Empty) == "empty", "describe empty")

    val circle = Shape.Circle(2.5)
    val circleDesc = WeaveFFI.describe(circle)
    expect(circleDesc == "circle(r=2.5)", "describe circle (got $circleDesc)")

    val rect = Shape.Rectangle(3.0f, 4.0f)
    val rectDesc = WeaveFFI.describe(rect)
    expect(rectDesc == "rectangle(3x4)", "describe rectangle (got $rectDesc)")

    val labeled = Shape.Labeled("hex", 6.toByte())
    val labeledDesc = WeaveFFI.describe(labeled)
    expect(labeledDesc == "labeled(hex x6)", "describe labeled (got $labeledDesc)")

    // scale: rich enum in and out; the return buffer decodes back into the
    // sealed hierarchy with the payload fields intact.
    val big = WeaveFFI.scale(circle, 4.0)
    expect(big is Shape.Circle, "scaled circle variant (got $big)")
    val bigRadius = (big as Shape.Circle).radius
    expect(abs(bigRadius - 10.0) < 1e-9, "scaled radius (got $bigRadius)")

    val bigRect = WeaveFFI.scale(rect, 2.0)
    expect(bigRect is Shape.Rectangle, "scaled rectangle variant (got $bigRect)")
    val r = bigRect as Shape.Rectangle
    expect(abs(r.width - 6.0f) < 1e-6f, "scaled width (got ${r.width})")
    expect(abs(r.height - 8.0f) < 1e-6f, "scaled height (got ${r.height})")

    // The string + u8 payload round-trips unchanged (scale leaves Labeled
    // alone), and data-class equality compares the decoded fields.
    val sameLabeled = WeaveFFI.scale(labeled, 2.0)
    expect(sameLabeled == labeled, "labeled round-trips (got $sameLabeled)")

    // The unit variant round-trips to the shared singleton.
    expect(WeaveFFI.scale(Shape.Empty, 2.0) == Shape.Empty, "empty round-trips")

    // A C-style enum keeps its plain int discriminants.
    expect(Channel.Green.value == 1, "Channel.Green == 1")
    expect(Channel.fromValue(2) == Channel.Blue, "Channel.fromValue(2) is Blue")

    // numerics: bytes in, u64 out. 250 wraps to a signed -6 byte but the JNI
    // shim reinterprets it as uint8_t, so the producer sums 250 * 4 == 1000.
    val total = WeaveFFI.sumBytes(
        byteArrayOf(250.toByte(), 250.toByte(), 250.toByte(), 250.toByte())
    )
    expect(total == 1000L, "sumBytes (got $total)")

    println("kotlin/shapes: OK")
}
