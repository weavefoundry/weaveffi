// Conformance consumer: shapes sample, Swift target.
//
// Binds through the generated `Shapes` module and drives the rich (algebraic)
// enum `Shape`: the opaque-object wrapper class, its nested `Tag` discriminant
// plus `tag` reader, the throwing per-variant static factories
// (`Shape.circle(...)`), and the per-variant field getters (`circleRadius`,
// `rectangleWidth`, `labeledLabel`, ...). Also exercises the free functions
// that take and return a `Shape` (describe/scale) and the expanded numerics
// (`sum_bytes`: [UInt8] in, UInt64 out). Mirrors the C and C++ consumers; exits
// non-zero on any mismatch and prints `swift/shapes: OK` on success.

import Foundation
import Shapes

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

do {
    // Empty (unit variant): tag only.
    let empty = try Shape.empty()
    expect(empty.tag == .empty, "empty tag (got \(empty.tag))")

    // Circle (f64 payload).
    let circle = try Shape.circle(2.5)
    expect(circle.tag == .circle, "circle tag (got \(circle.tag))")
    expect(abs(circle.circleRadius - 2.5) < 1e-9, "circle radius (got \(circle.circleRadius))")

    // Rectangle (two f32 payloads).
    let rect = try Shape.rectangle(3.0, 4.0)
    expect(rect.tag == .rectangle, "rectangle tag (got \(rect.tag))")
    expect(abs(rect.rectangleWidth - 3.0) < 1e-6, "rectangle width (got \(rect.rectangleWidth))")
    expect(abs(rect.rectangleHeight - 4.0) < 1e-6, "rectangle height (got \(rect.rectangleHeight))")

    // Labeled (string + u8 payload).
    let labeled = try Shape.labeled("hex", 6)
    expect(labeled.tag == .labeled, "labeled tag (got \(labeled.tag))")
    expect(labeled.labeledLabel == "hex", "labeled label (got \(labeled.labeledLabel))")
    expect(labeled.labeledCount == 6, "labeled count (got \(labeled.labeledCount))")

    // describe: dispatch on the active variant.
    let desc = try Shapes.shapes_describe(circle)
    expect(desc == "circle(r=2.5)", "describe (got \(desc))")

    // scale: rich enum in and out.
    let big = try Shapes.shapes_scale(circle, 4.0)
    expect(big.tag == .circle, "scaled tag (got \(big.tag))")
    expect(abs(big.circleRadius - 10.0) < 1e-9, "scaled radius (got \(big.circleRadius))")

    // numerics: [UInt8] in, UInt64 out.
    let total = try Shapes.shapes_sum_bytes([250, 250, 250, 250])
    expect(total == 1000, "sum_bytes (got \(total))")

    print("swift/shapes: OK")
} catch {
    fail("threw: \(error)")
}
