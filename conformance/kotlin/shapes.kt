// Conformance consumer: shapes sample, Android/Kotlin (JNI) target.
//
// Exercises the rich (algebraic) enum surface the JNI layer now emits: the owned
// `Shape` handle-wrapper class, its per-variant factories (`Shape.circle(...)`),
// the nested `Tag` discriminant + `tag` reader, and the per-variant field
// getters (`circleRadius`, `rectangleWidth`/`rectangleHeight`, `labeledLabel`/
// `labeledCount`) over every variant: unit (`Empty`), f64 (`Circle`), two f32
// (`Rectangle`), and string + u8 (`Labeled`). Also drives the free functions
// that take and return a `Shape` by opaque handle (`describe`, `scale`) plus the
// expanded numerics (`sum_bytes`: list<u8> in, u64 out). Mirrors the C and C++
// consumers' assertions. Compiled in-module with the generated `WeaveFFI.kt`, so
// the `internal` constructor used to re-wrap returned handles is reachable.
@file:JvmName("Main")

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
    // Empty (unit variant): tag only, no payload.
    val empty = Shape.empty()
    expect(empty.tag == Shape.Tag.Empty, "empty tag")

    // Circle (f64 payload).
    val circle = Shape.circle(2.5)
    expect(circle.tag == Shape.Tag.Circle, "circle tag")
    expect(abs(circle.circleRadius - 2.5) < 1e-9, "circle radius (got ${circle.circleRadius})")

    // Rectangle (two f32 payloads).
    val rect = Shape.rectangle(3.0f, 4.0f)
    expect(rect.tag == Shape.Tag.Rectangle, "rectangle tag")
    expect(abs(rect.rectangleWidth - 3.0f) < 1e-6f, "rectangle width (got ${rect.rectangleWidth})")
    expect(abs(rect.rectangleHeight - 4.0f) < 1e-6f, "rectangle height (got ${rect.rectangleHeight})")

    // Labeled (string + u8 payload).
    val labeled = Shape.labeled("hex", 6.toByte())
    expect(labeled.tag == Shape.Tag.Labeled, "labeled tag")
    expect(labeled.labeledLabel == "hex", "labeled label (got ${labeled.labeledLabel})")
    expect(labeled.labeledCount.toInt() == 6, "labeled count (got ${labeled.labeledCount})")

    // describe: dispatch on the active variant, rich enum in -> string out.
    val desc = WeaveFFI.shapes_describe(circle)
    expect(desc == "circle(r=2.5)", "describe circle (got $desc)")

    // scale: rich enum in and out (opaque handle round-trips through the class).
    val big = WeaveFFI.shapes_scale(circle, 4.0)
    expect(big.tag == Shape.Tag.Circle, "scaled tag")
    expect(abs(big.circleRadius - 10.0) < 1e-9, "scaled radius (got ${big.circleRadius})")

    // numerics: list<u8> in, u64 out. 250 wraps to a signed -6 byte but the JNI
    // shim reinterprets it as uint8_t, so the producer sums 250 * 4 == 1000.
    val total = WeaveFFI.shapes_sum_bytes(
        byteArrayOf(250.toByte(), 250.toByte(), 250.toByte(), 250.toByte())
    )
    expect(total == 1000L, "sum_bytes (got $total)")

    // Release the native handles (also exercises close()/nativeDestroy).
    big.close()
    labeled.close()
    rect.close()
    circle.close()
    empty.close()

    println("kotlin/shapes: OK")
}
