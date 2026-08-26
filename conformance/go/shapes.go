// Conformance consumer: shapes sample, Go target.
//
// Drives the generated cgo bindings for rich (algebraic) enums as a sealed
// sum type: the Shape interface with one value struct per variant
// (ShapeEmpty, ShapeCircle, ...), constructed as plain Go composite literals
// and inspected with type assertions, plus the free functions that pack a
// Shape into a value buffer on the way in and unpack one on the way out. The
// free functions are non-throwing, so they have plain returns. Also covers
// the expanded numerics (f32 fields, u8 field, []byte in, uint64 out). Exits
// 0 on success; aborts (non-zero) on any failed assertion.

package main

import (
	"fmt"
	"math"
	"os"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

func main() {
	// Unit variant: an empty struct satisfying the sealed interface.
	var empty wv.Shape = wv.ShapeEmpty{}
	_, isEmpty := empty.(wv.ShapeEmpty)
	expect(isEmpty, "empty is ShapeEmpty")

	// f64 payload.
	circle := wv.ShapeCircle{Radius: 2.5}
	expect(math.Abs(circle.Radius-2.5) < 1e-9, "circle radius 2.5")

	// Two f32 payloads.
	rect := wv.ShapeRectangle{Width: 3.0, Height: 4.0}
	expect(math.Abs(float64(rect.Width)-3.0) < 1e-6, "rectangle width 3.0")
	expect(math.Abs(float64(rect.Height)-4.0) < 1e-6, "rectangle height 4.0")

	// string + u8 payload.
	labeled := wv.ShapeLabeled{Label: "hex", Count: 6}
	expect(labeled.Label == "hex", "labeled label \"hex\"")
	expect(labeled.Count == 6, "labeled count 6")

	// Free functions (non-throwing, plain returns): Shape in, string/Shape
	// out. Each call round-trips the variant through the value buffer.
	expect(wv.Describe(empty) == "empty", "describe(empty) == \"empty\"")
	expect(wv.Describe(circle) == "circle(r=2.5)", "describe(circle) == \"circle(r=2.5)\"")
	expect(wv.Describe(rect) == "rectangle(3x4)", "describe(rect) == \"rectangle(3x4)\"")
	expect(wv.Describe(labeled) == "labeled(hex x6)", "describe(labeled) == \"labeled(hex x6)\"")

	big := wv.Scale(circle, 4.0)
	bigCircle, isCircle := big.(wv.ShapeCircle)
	expect(isCircle, "scaled shape is ShapeCircle")
	expect(math.Abs(bigCircle.Radius-10.0) < 1e-9, "scaled radius 10.0")

	// Scaling the unit variant round-trips the tag-only encoding.
	scaledEmpty := wv.Scale(empty, 2.0)
	_, isEmpty = scaledEmpty.(wv.ShapeEmpty)
	expect(isEmpty, "scaled empty stays ShapeEmpty")

	// A type switch covers every variant of the sealed sum type. Scaling a
	// labeled shape leaves its payload untouched, so the string + u8 fields
	// round-trip through the buffer unchanged.
	scaledLabeled := wv.Scale(labeled, 2.0)
	switch s := scaledLabeled.(type) {
	case wv.ShapeLabeled:
		expect(s.Label == "hex", "scaled labeled keeps its label")
		expect(s.Count == 6, "scaled labeled keeps its count")
	case wv.ShapeEmpty, wv.ShapeCircle, wv.ShapeRectangle:
		expect(false, "scale(labeled) returned the wrong variant")
	}

	// A C-style enum keeps its plain int32 constants.
	expect(wv.ChannelGreen == 1, "Channel constants keep their discriminants")

	// Numerics: list<u8> in, u64 out.
	total := wv.SumBytes([]byte{250, 250, 250, 250})
	expect(total == 1000, "sum_bytes == 1000")

	fmt.Println("go/shapes: OK")
}
