// Conformance consumer: shapes sample, Swift target.
//
// Binds through the generated `Shapes` module and drives the rich (algebraic)
// enum `Shape`, now a native Swift enum with associated values: unit variant
// (`.empty`), single payload (`.circle(radius:)`), multiple payloads
// (`.rectangle(width:height:)`), and mixed string + u8 payloads
// (`.labeled(label:count:)`). Variants are constructed locally (no factories,
// no `try`) and destructured with pattern matching; the value-buffer encoding
// is exercised by the free functions that take and return a `Shape`
// (`Shapes.describe(shape:)` and `Shapes.scale(shape:factor:)`, non-throwing
// and called without `try`) plus the expanded numerics (`sumBytes`: Data in,
// UInt64 out). Mirrors the C and C++ consumers; exits non-zero on any
// mismatch and prints `swift/shapes: OK` on success.

import Foundation
import Shapes

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

// Empty (unit variant): tag only. `describe` proves the encoded tag reached
// the producer intact.
let empty = Shape.empty
guard case .empty = empty else { fail("empty variant") }
expect(Shapes.describe(shape: empty) == "empty", "describe empty")

// Circle (f64 payload) round-trips through the producer and back.
let circle = Shape.circle(radius: 2.5)
guard case let .circle(radius) = circle else { fail("circle variant") }
expect(abs(radius - 2.5) < 1e-9, "circle radius (got \(radius))")

// Rectangle (two f32 payloads).
let rect = Shape.rectangle(width: 3.0, height: 4.0)
guard case let .rectangle(width, height) = rect else { fail("rectangle variant") }
expect(abs(width - 3.0) < 1e-6, "rectangle width (got \(width))")
expect(abs(height - 4.0) < 1e-6, "rectangle height (got \(height))")

// Labeled (string + u8 payload).
let labeled = Shape.labeled(label: "hex", count: 6)
guard case let .labeled(label, count) = labeled else { fail("labeled variant") }
expect(label == "hex", "labeled label (got \(label))")
expect(count == 6, "labeled count (got \(count))")

// describe: the producer dispatches on the active variant; non-throwing,
// no `try`.
let desc = Shapes.describe(shape: circle)
expect(desc == "circle(r=2.5)", "describe (got \(desc))")

// scale: rich enum in and out, decoded back into a Swift enum value.
let big = Shapes.scale(shape: circle, factor: 4.0)
guard case let .circle(bigRadius) = big else { fail("scaled variant (got \(big))") }
expect(abs(bigRadius - 10.0) < 1e-9, "scaled radius (got \(bigRadius))")

// A multi-payload variant survives the round trip too.
let bigRect = Shapes.scale(shape: rect, factor: 2.0)
guard case let .rectangle(w2, h2) = bigRect else { fail("scaled rect variant (got \(bigRect))") }
expect(abs(w2 - 6.0) < 1e-6 && abs(h2 - 8.0) < 1e-6, "scaled rect (got \(w2)x\(h2))")

// numerics: Data in, UInt64 out.
let total = Shapes.sumBytes(values: Data([250, 250, 250, 250]))
expect(total == 1000, "sum_bytes (got \(total))")

print("swift/shapes: OK")
