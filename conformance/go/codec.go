// Conformance consumer: codec sample, Go target (ABI revision 4).
//
// The shared-vector loop: for every vector the library serves, decode it
// through the generated bindings, hand it back to CheckVector (which must
// accept it and reject it at the neighboring index), and push each primitive
// vector's value through the matching direct-family Echo function. Then
// vectors built from literals (so a symmetric encode/decode bug can't hide),
// spot checks of decoded fields, the typed *OutOfRangeError and its payload,
// malformed input rejected (an undeclared enum value, text that isn't UTF-8,
// a nil sum-type value the encoder refuses), and object identity and
// reference counting through buffers. Ends by asserting the library's leak
// counters are zero.

package main

import (
	"bytes"
	"fmt"
	"math"

	codec "__MODPATH__"
)

func sameF32(a, b float32) bool { return math.Float32bits(a) == math.Float32bits(b) }
func sameF64(a, b float64) bool { return math.Float64bits(a) == math.Float64bits(b) }

func fetch(i uint32) codec.Vector {
	v, err := codec.CodecVector(i)
	expect(err == nil && v != nil, fmt.Sprintf("vector(%d): %v", i, err))
	return v
}

func vectorName(i uint32) string {
	name, err := codec.VectorName(i)
	expect(err == nil, fmt.Sprintf("vector_name(%d): %v", i, err))
	return name
}

// find returns the index of the vector called name.
func find(n uint32, name string) uint32 {
	for i := range n {
		if vectorName(i) == name {
			return i
		}
	}
	expect(false, "no vector named "+name)
	return 0
}

// release closes every token a decoded vector holds.
func release(v codec.Vector) {
	if o, ok := v.(codec.VectorObjects); ok {
		closeHolder(o.Value)
	}
}

func closeHolder(h codec.Holder) {
	h.Primary.Close()
	if h.Spare != nil {
		h.Spare.Close()
	}
	for _, t := range h.Many {
		t.Close()
	}
	for _, t := range h.ByName {
		t.Close()
	}
}

// echo pushes a primitive vector's value through its direct-family echo.
func echo(v codec.Vector) {
	switch x := v.(type) {
	case codec.VectorI8:
		expect(codec.EchoI8(x.Value) == x.Value, "echo i8")
	case codec.VectorU8:
		expect(codec.EchoU8(x.Value) == x.Value, "echo u8")
	case codec.VectorI16:
		expect(codec.EchoI16(x.Value) == x.Value, "echo i16")
	case codec.VectorU16:
		expect(codec.EchoU16(x.Value) == x.Value, "echo u16")
	case codec.VectorI32:
		expect(codec.EchoI32(x.Value) == x.Value, "echo i32")
	case codec.VectorU32:
		expect(codec.EchoU32(x.Value) == x.Value, "echo u32")
	case codec.VectorI64:
		expect(codec.EchoI64(x.Value) == x.Value, "echo i64")
	case codec.VectorU64:
		expect(codec.EchoU64(x.Value) == x.Value, "echo u64")
	case codec.VectorF32:
		expect(sameF32(codec.EchoF32(x.Value), x.Value), "echo f32 (bitwise)")
	case codec.VectorF64:
		expect(sameF64(codec.EchoF64(x.Value), x.Value), "echo f64 (bitwise)")
	case codec.VectorFlag:
		expect(codec.EchoBool(x.Value) == x.Value, "echo bool")
	case codec.VectorText:
		expect(codec.EchoText(x.Value) == x.Value, fmt.Sprintf("echo text %q", x.Value))
	case codec.VectorBlob:
		expect(bytes.Equal(codec.EchoBlob(x.Value), x.Value), "echo blob")
	case codec.VectorHue:
		expect(codec.EchoColor(x.Value) == x.Value, "echo color")
	}
}

func everyVector(n uint32) {
	for i := range n {
		v := fetch(i)
		if !codec.CheckVector(i, v) {
			expect(false, fmt.Sprintf("vector %d (%s) did not round-trip; the library saw %s",
				i, vectorName(i), codec.DescribeVector(v)))
		}
		expect(!codec.CheckVector((i+1)%n, v), fmt.Sprintf("vector %d matches its neighbor", i))
		echo(v)
		release(v)
	}
}

func canonicalScalars() codec.Scalars {
	return codec.Scalars{
		I8Value:  -8,
		U8Value:  200,
		I16Value: -16000,
		U16Value: 60000,
		I32Value: -2000000000,
		U32Value: 4000000000,
		I64Value: -9007199254740993,
		U64Value: math.MaxUint64,
		F32Value: 1.5,
		F64Value: -2.25e100,
		Flag:     true,
		Color:    codec.ColorBlue,
	}
}

func literalVectors(n uint32) {
	check := func(name string, v codec.Vector) bool { return codec.CheckVector(find(n, name), v) }

	s := canonicalScalars()
	expect(check("scalars canonical", codec.VectorAllScalars{Value: s}), "scalars canonical")
	s.U16Value = 60001
	expect(!check("scalars canonical", codec.VectorAllScalars{Value: s}), "one changed field")

	labeled := codec.VectorFigure{Value: codec.ShapeLabeled{Label: "tag", Count: 3}}
	expect(check("shape labeled", labeled), "shape labeled")
	expect(check("string interior nul", codec.VectorText{Value: "nul\x00inside\x00"}), "interior nul")

	// Any NaN matches the NaN vector; zero keeps its sign.
	nan := math.Float64frombits(0x7ff8000000000001)
	expect(check("f64 nan", codec.VectorF64{Value: nan}), "a NaN payload")
	expect(check("f64 -0", codec.VectorF64{Value: math.Copysign(0, -1)}), "-0")
	expect(!check("f64 -0", codec.VectorF64{Value: 0}), "+0 is not -0")

	expect(check("u64 max", codec.VectorU64{Value: math.MaxUint64}), "u64 max")
	expect(check("enum infrared", codec.VectorHue{Value: codec.ColorInfrared}), "enum infrared")

	zero := int64(0)
	expect(check("optional zero", codec.VectorMaybeI64{Value: &zero}), "Some(0)")
	expect(check("optional absent", codec.VectorMaybeI64{Value: nil}), "None")
	expect(!check("optional zero", codec.VectorMaybeI64{Value: nil}), "None is not Some(0)")

	// A map's entry order doesn't matter on the wire.
	counts := map[string]int64{"x": 0, "héllo": -1, "": math.MaxInt64}
	expect(check("map of strings", codec.VectorCounts{Value: counts}), "map of strings")
	expect(check("blank", codec.VectorBlank{}), "blank")

	// The sparse holder, from a consumer-made token.
	lone := codec.NewToken(-1)
	sparse := codec.VectorObjects{Value: codec.Holder{Primary: lone}}
	expect(check("objects sparse", sparse), "objects sparse")
	lone.Close()
}

func spotChecks(n uint32) {
	v := fetch(find(n, "i64 past 2^53")).(codec.VectorI64)
	expect(v.Value == -9007199254740993, "i64 past 2^53 is exact")

	f := fetch(find(n, "f32 min subnormal")).(codec.VectorF32)
	expect(math.Float32bits(f.Value) == 1, "f32 min subnormal bits")

	t := fetch(find(n, "string astral")).(codec.VectorText)
	expect(t.Value == "🦀 crab 😀", "string astral")

	m := fetch(find(n, "scalars minimum")).(codec.VectorAllScalars).Value
	expect(m.I8Value == math.MinInt8 && m.I16Value == math.MinInt16 && m.I32Value == math.MinInt32, "minimum signed")
	expect(m.I64Value == math.MinInt64 && m.U8Value == 0 && m.U16Value == 0 && m.U32Value == 0 && m.U64Value == 0, "minimum")
	expect(math.IsInf(float64(m.F32Value), -1) && math.IsNaN(m.F64Value), "minimum floats")
	expect(m.Color == codec.ColorInfrared && !m.Flag, "minimum enum and flag")

	c := fetch(find(n, "composite canonical")).(codec.VectorDeep).Value
	expect(c.Name == "héllo wörld ✓", "name")
	expect(len(c.Blob) == 6 && c.Blob[5] == 255, "blob")
	expect(c.SomeI64 != nil && *c.SomeI64 == math.MinInt64 && c.NoneI64 == nil, "optional i64s")
	expect(c.SomeText != nil && *c.SomeText == "", "present empty text")
	expect(len(c.Names) == 3 && c.Names[1] == "", "names")
	expect(len(c.Matrix) == 3 && len(c.Matrix[1]) == 0 && c.Matrix[2][0] == -4, "matrix")
	expect(len(c.Floats) == 6 && math.IsNaN(c.Floats[0]) && math.Signbit(c.Floats[3]), "floats")
	expect(len(c.ByName) == 4 && len(c.ByID) == 3 && len(c.ByColor) == 2 && len(c.Flags) == 2, "maps")
	expect(c.Scalars.U32Value == 4000000000, "nested scalars")
	shape, ok := c.Shape.(codec.ShapeLabeled)
	expect(ok && shape.Count == 3, "shape")
	nested, ok := c.Shapes[5].(codec.ShapeNested)
	expect(len(c.Shapes) == 6 && ok && nested.Note == nil, "shapes")
	_, ok = c.MaybeShape.(codec.ShapeNested)
	expect(ok, "maybe shape")
	expect(c.MaybeList != nil && len(c.MaybeList) == 2, "maybe list")
	expect(len(c.Sparse) == 3 && c.Sparse[1] == nil && *c.Sparse[0], "sparse")
	expect(len(c.Colors) == 4 && c.Colors[3] == codec.ColorInfrared, "colors")
}

func outOfRange(n uint32) {
	v, err := codec.CodecVector(n)
	expect(v == nil, "a failed vector call returns nil")
	e := expectAs[*codec.OutOfRangeError](err, "vector(n)")
	expect(e.Index == n && e.Count == n, fmt.Sprintf("payload index %d count %d", e.Index, e.Count))
	expect(e.Error() == fmt.Sprintf("vector %d is out of range (count %d)", n, n), "message: "+e.Error())
	expect(e.Code() == 1, "code")
	expectAs[codec.CodecError](err, "an OutOfRangeError is a CodecError")

	_, err = codec.VectorName(n + 5)
	e = expectAs[*codec.OutOfRangeError](err, "vector_name(n + 5)")
	expect(e.Index == n+5 && e.Count == n, "vector_name payload")

	expect(!codec.CheckVector(n, codec.VectorBlank{}), "check_vector past the end")
}

// expectMarshalFailure asserts that f panics with the runtime error for a
// value the library can't take (-3).
func expectMarshalFailure(what string, f func()) {
	r := catchPanic(f)
	e, ok := r.(*codec.Error)
	expect(ok && e.Code == -3, fmt.Sprintf("%s: want a -3 *Error panic, got %v", what, r))
}

func malformed() {
	// Go can spell values the library rejects: an undeclared enum value and
	// a string that isn't UTF-8, directly and inside a buffer.
	expectMarshalFailure("echo_color(3)", func() { codec.EchoColor(codec.Color(3)) })
	expectMarshalFailure("echo_text(bad UTF-8)", func() { codec.EchoText("\xc3\x28") })
	expectMarshalFailure("check_vector(Hue 3)", func() { codec.CheckVector(0, codec.VectorHue{Value: 3}) })
	expectMarshalFailure("check_vector(bad UTF-8)", func() {
		codec.CheckVector(0, codec.VectorText{Value: "\xc3\x28"})
	})
	expect(codec.EchoText("") == "", "empty text")

	// The encoder refuses a nil sum-type value instead of writing a bad tag.
	expect(catchPanic(func() { codec.CheckVector(0, nil) }) != nil, "nil Vector")
	expect(catchPanic(func() { codec.CheckVector(0, codec.VectorFigure{}) }) != nil, "nil Shape")
}

func objects(n uint32) {
	// The full object vector decodes to live tokens with the table's values.
	full := fetch(find(n, "objects full")).(codec.VectorObjects).Value
	expect(full.Primary.Value() == 10 && full.Spare != nil && full.Spare.Value() == 11, "primary and spare")
	expect(len(full.Many) == 3 && full.Many[2].Value() == math.MinInt64, "many")
	expect(len(full.ByName) == 2 && full.ByName["b"].Value() == 21, "by name")
	// Each encoding mints fresh references, so the holder can be sent twice.
	want := int64(10+11+12+13+20+21) + math.MinInt64
	expect(codec.SumHolder(full) == want && codec.SumHolder(full) == want, "sum_holder twice")

	// primary_of returns the very same object: identity, not value.
	p := codec.PrimaryOf(full)
	expect(p.Value() == 10, "primary_of value")
	expect(codec.SamePrimary(full, codec.Holder{Primary: p}), "primary_of is the same object")
	twin := codec.NewToken(10)
	expect(!codec.SamePrimary(full, codec.Holder{Primary: twin}), "an equal value is another object")

	// A holder built from consumer tokens, one wrapper in several slots.
	minus := codec.NewToken(-4)
	mine := codec.Holder{
		Primary: twin,
		Spare:   twin,
		Many:    []*codec.Token{twin, twin, minus},
		ByName:  map[string]*codec.Token{"k": twin},
	}
	expect(codec.SumHolder(mine) == 10*5-4, "consumer holder sum")

	// Close is idempotent; a closed wrapper can't be used.
	minus.Close()
	minus.Close()
	expect(catchPanic(func() { minus.Value() }) != nil, "use after Close panics")

	twin.Close()
	p.Close()
	closeHolder(full)
}

func main() {
	n := codec.VectorCount()
	expect(n >= 60, fmt.Sprintf("vector_count() = %d", n))

	everyVector(n)
	literalVectors(n)
	spotChecks(n)
	outOfRange(n)
	malformed()
	objects(n)

	expectNoLeaks(codec.DebugLive)
	fmt.Printf("go/codec: OK (%d vectors)\n", n)
}
