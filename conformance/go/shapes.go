// Conformance consumer: shapes sample, Go target.
//
// Drives the generated cgo bindings for rich (algebraic) enums: the opaque
// Shape wrapper owning the C handle (freed via Close), its int32 Tag() reader
// and exported per-variant tag constants, the per-variant New* constructors
// (NewShapeCircle, ...) and variant-namespaced field accessors (CircleRadius,
// ...), plus the free functions that take and return Shape. The free
// functions are non-throwing, so they have plain returns; only the variant
// constructors keep (T, error) for construction plumbing. Also covers the
// expanded numerics (f32 fields, u8 field, []byte in, uint64 out). Exits 0 on
// success; aborts (non-zero) on any failed assertion.

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
	// Unit variant: tag only.
	empty, err := wv.NewShapeEmpty()
	expect(err == nil, "new empty")
	expect(empty.Tag() == wv.ShapeEmpty, "empty tag == 0")

	// f64 payload.
	circle, err := wv.NewShapeCircle(2.5)
	expect(err == nil, "new circle")
	expect(circle.Tag() == wv.ShapeCircle, "circle tag")
	expect(math.Abs(circle.CircleRadius()-2.5) < 1e-9, "circle radius 2.5")

	// Two f32 payloads.
	rect, err := wv.NewShapeRectangle(3.0, 4.0)
	expect(err == nil, "new rectangle")
	expect(rect.Tag() == wv.ShapeRectangle, "rectangle tag")
	expect(math.Abs(float64(rect.RectangleWidth())-3.0) < 1e-6, "rectangle width 3.0")
	expect(math.Abs(float64(rect.RectangleHeight())-4.0) < 1e-6, "rectangle height 4.0")

	// string + u8 payload.
	labeled, err := wv.NewShapeLabeled("hex", 6)
	expect(err == nil, "new labeled")
	expect(labeled.Tag() == wv.ShapeLabeled, "labeled tag")
	expect(labeled.LabeledLabel() == "hex", "labeled label \"hex\"")
	expect(labeled.LabeledCount() == 6, "labeled count 6")

	// Free functions (non-throwing, plain returns): Shape in, string/Shape out.
	desc := wv.Describe(circle)
	expect(desc == "circle(r=2.5)", "describe(circle) == \"circle(r=2.5)\"")

	big := wv.Scale(circle, 4.0)
	expect(big.Tag() == wv.ShapeCircle, "scaled tag")
	expect(math.Abs(big.CircleRadius()-10.0) < 1e-9, "scaled radius 10.0")

	// Numerics: list<u8> in, u64 out.
	total := wv.SumBytes([]byte{250, 250, 250, 250})
	expect(total == 1000, "sum_bytes == 1000")

	big.Close()
	labeled.Close()
	rect.Close()
	circle.Close()
	empty.Close()

	fmt.Println("go/shapes: OK")
}
