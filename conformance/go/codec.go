// Conformance consumer: codec sample, Go target.
//
// Drives the value-buffer round-trip oracle through the generated cgo
// bindings. For Scalars and Composite it fetches the producer's canonical
// fixture (producer encodes, Go decodes), checks every field against the
// values in samples/codec/src/lib.rs, hands the fixture back to verify_*
// (Go encodes, producer decodes and compares), and re-encodes through
// roundtrip_* comparing field by field. It then builds its own values with
// edge cases (empty strings, lists and maps, embedded NUL and non-BMP text,
// int64/uint64 extremes, NaN, the infinities, negative zero, present-but-
// empty optionals, lists of absent optionals) and round-trips them, exercises
// every Shape variant, the top-level optional/list/map/string/bytes/scalar
// echoes, the typed CodecError on a mismatch, and Holder: object tokens in a
// record field, an optional, and a list, with primary_of returning a wrapper
// to the same object, same_primary proving identity, and every wrapper
// released (double Close harmless, use after Close trapped). Exits 0 on
// success; aborts (non-zero) on any mismatch.

package main

import (
	"errors"
	"fmt"
	"math"
	"os"
	"strings"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

// Bit-exact float comparison so NaN payloads and the sign of zero count.
func sameF64(a, b float64) bool { return math.Float64bits(a) == math.Float64bits(b) }
func sameF32(a, b float32) bool { return math.Float32bits(a) == math.Float32bits(b) }

func sameStrPtr(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func sameI64Ptr(a, b *int64) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func sameBytes(a, b []byte) bool {
	if (a == nil) != (b == nil) || len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func sameScalars(a, b wv.Scalars) bool {
	return a.I8Value == b.I8Value && a.U8Value == b.U8Value &&
		a.I16Value == b.I16Value && a.U16Value == b.U16Value &&
		a.I32Value == b.I32Value && a.U32Value == b.U32Value &&
		a.I64Value == b.I64Value && a.U64Value == b.U64Value &&
		sameF32(a.F32Value, b.F32Value) && sameF64(a.F64Value, b.F64Value) &&
		a.Flag == b.Flag && a.Color == b.Color
}

func sameShape(a, b wv.Shape) bool {
	switch x := a.(type) {
	case nil:
		return b == nil
	case wv.ShapeEmpty:
		_, ok := b.(wv.ShapeEmpty)
		return ok
	case wv.ShapeCircle:
		y, ok := b.(wv.ShapeCircle)
		return ok && sameF64(x.Radius, y.Radius)
	case wv.ShapeRect:
		y, ok := b.(wv.ShapeRect)
		return ok && sameF32(x.Width, y.Width) && sameF32(x.Height, y.Height)
	case wv.ShapeLabeled:
		y, ok := b.(wv.ShapeLabeled)
		return ok && x.Label == y.Label && x.Count == y.Count
	case wv.ShapeNested:
		y, ok := b.(wv.ShapeNested)
		return ok && sameScalars(x.Inner, y.Inner) && sameStrPtr(x.Note, y.Note)
	}
	return false
}

func sameComposite(a, b wv.Composite) (bool, string) {
	switch {
	case a.Name != b.Name:
		return false, "name"
	case !sameBytes(a.Blob, b.Blob):
		return false, "blob"
	case !sameI64Ptr(a.SomeI64, b.SomeI64):
		return false, "some_i64"
	case !sameI64Ptr(a.NoneI64, b.NoneI64):
		return false, "none_i64"
	case !sameStrPtr(a.SomeText, b.SomeText):
		return false, "some_text"
	case len(a.Names) != len(b.Names):
		return false, "names"
	case len(a.Matrix) != len(b.Matrix):
		return false, "matrix"
	case len(a.Empty) != len(b.Empty):
		return false, "empty"
	case len(a.ByName) != len(b.ByName):
		return false, "by_name"
	case len(a.ById) != len(b.ById):
		return false, "by_id"
	case !sameScalars(a.Scalars, b.Scalars):
		return false, "scalars"
	case !sameShape(a.Shape, b.Shape):
		return false, "shape"
	case len(a.Shapes) != len(b.Shapes):
		return false, "shapes"
	case !sameShape(a.MaybeShape, b.MaybeShape):
		return false, "maybe_shape"
	case !sameBytes(a.MaybeList, b.MaybeList):
		return false, "maybe_list"
	case len(a.Sparse) != len(b.Sparse):
		return false, "sparse"
	case len(a.Colors) != len(b.Colors):
		return false, "colors"
	}
	for i := range a.Names {
		if a.Names[i] != b.Names[i] {
			return false, fmt.Sprintf("names[%d]", i)
		}
	}
	for i := range a.Matrix {
		if len(a.Matrix[i]) != len(b.Matrix[i]) {
			return false, fmt.Sprintf("matrix[%d]", i)
		}
		for j := range a.Matrix[i] {
			if a.Matrix[i][j] != b.Matrix[i][j] {
				return false, fmt.Sprintf("matrix[%d][%d]", i, j)
			}
		}
	}
	for i := range a.Empty {
		if !sameF64(a.Empty[i], b.Empty[i]) {
			return false, fmt.Sprintf("empty[%d]", i)
		}
	}
	for k, v := range a.ByName {
		if w, ok := b.ByName[k]; !ok || v != w {
			return false, "by_name[" + k + "]"
		}
	}
	for k, v := range a.ById {
		if w, ok := b.ById[k]; !ok || !sameScalars(v, w) {
			return false, fmt.Sprintf("by_id[%d]", k)
		}
	}
	for i := range a.Shapes {
		if !sameShape(a.Shapes[i], b.Shapes[i]) {
			return false, fmt.Sprintf("shapes[%d]", i)
		}
	}
	for i := range a.Sparse {
		x, y := a.Sparse[i], b.Sparse[i]
		if (x == nil) != (y == nil) || (x != nil && *x != *y) {
			return false, fmt.Sprintf("sparse[%d]", i)
		}
	}
	for i := range a.Colors {
		if a.Colors[i] != b.Colors[i] {
			return false, fmt.Sprintf("colors[%d]", i)
		}
	}
	return true, ""
}

func expectSameComposite(a, b wv.Composite, what string) {
	same, field := sameComposite(a, b)
	expect(same, fmt.Sprintf("%s: field %s differs\n  a: %+v\n  b: %+v", what, field, a, b))
}

func catchPanic(f func()) (v any) {
	defer func() { v = recover() }()
	f()
	return nil
}

func ptrI64(v int64) *int64   { return &v }
func ptrStr(v string) *string { return &v }
func ptrBool(v bool) *bool    { return &v }

// canonicalScalars mirrors `canonical_scalars()` in the producer.
func canonicalScalars() wv.Scalars {
	return wv.Scalars{
		I8Value:  -8,
		U8Value:  200,
		I16Value: -16_000,
		U16Value: 60_000,
		I32Value: -2_000_000_000,
		U32Value: 4_000_000_000,
		I64Value: -9_007_199_254_740_993,
		U64Value: 18_446_744_073_709_551_615,
		F32Value: 1.5,
		F64Value: -2.25e100,
		Flag:     true,
		Color:    wv.ColorBlue,
	}
}

func main() {
	// ── Scalars: producer encodes, Go decodes ──
	s := wv.SampleScalars()
	expect(sameScalars(s, canonicalScalars()), fmt.Sprintf("sample_scalars matches the fixture (got %+v)", s))
	expect(s.Color == 7, "Blue keeps its explicit discriminant 7")

	// Go encodes, producer decodes and compares.
	ok, err := wv.VerifyScalars(s)
	expect(err == nil && ok, fmt.Sprintf("verify_scalars(sample) (err %v)", err))
	back := wv.RoundtripScalars(s)
	expect(sameScalars(back, s), fmt.Sprintf("roundtrip_scalars echoes the sample (got %+v)", back))

	// A mismatch is the typed CodecError.
	changed := s
	changed.U64Value--
	ok, err = wv.VerifyScalars(changed)
	var cerr *wv.CodecError
	expect(!ok && errors.As(err, &cerr), fmt.Sprintf("verify_scalars(changed) yields *CodecError (got %T %v)", err, err))
	expect(cerr.Code == wv.CodecErrorMismatch, fmt.Sprintf("mismatch code (got %d)", cerr.Code))
	expect(cerr.Code == 1, fmt.Sprintf("mismatch code is 1 (got %d)", cerr.Code))
	expect(cerr.Message == "value does not match the canonical fixture", fmt.Sprintf("mismatch default message (got %q)", cerr.Message))

	// Extremes and IEEE specials through the buffered record.
	extreme := wv.Scalars{
		I8Value:  math.MinInt8,
		U8Value:  math.MaxUint8,
		I16Value: math.MinInt16,
		U16Value: math.MaxUint16,
		I32Value: math.MinInt32,
		U32Value: math.MaxUint32,
		I64Value: math.MaxInt64,
		U64Value: math.MaxUint64,
		F32Value: float32(math.NaN()),
		F64Value: math.Copysign(0, -1),
		Flag:     false,
		Color:    wv.ColorRed,
	}
	back = wv.RoundtripScalars(extreme)
	expect(sameScalars(back, extreme), fmt.Sprintf("roundtrip_scalars keeps extremes, NaN, and -0 (got %+v)", back))
	expect(math.IsNaN(float64(back.F32Value)) && math.Signbit(back.F64Value) && back.F64Value == 0,
		"NaN and negative zero decode as such")
	extreme.I64Value, extreme.F64Value, extreme.F32Value = math.MinInt64, math.Inf(-1), float32(math.Inf(1))
	back = wv.RoundtripScalars(extreme)
	expect(sameScalars(back, extreme), "roundtrip_scalars keeps MinInt64 and the infinities")

	// ── Composite: producer encodes, Go decodes ──
	c := wv.SampleComposite()
	expect(c.Name == "héllo wörld ✓", fmt.Sprintf("name (got %q)", c.Name))
	expect(sameBytes(c.Blob, []byte{0, 1, 2, 253, 254, 255}), fmt.Sprintf("blob (got %v)", c.Blob))
	expect(c.SomeI64 != nil && *c.SomeI64 == math.MinInt64, "some_i64 == i64::MIN")
	expect(c.NoneI64 == nil, "none_i64 absent")
	expect(c.SomeText != nil && *c.SomeText == "", "some_text is a present empty string")
	expect(len(c.Names) == 3 && c.Names[0] == "a" && c.Names[1] == "" && c.Names[2] == "ccc",
		fmt.Sprintf("names (got %q)", c.Names))
	expect(len(c.Matrix) == 3 && len(c.Matrix[0]) == 3 && c.Matrix[0][2] == 3 &&
		len(c.Matrix[1]) == 0 && len(c.Matrix[2]) == 1 && c.Matrix[2][0] == -4,
		fmt.Sprintf("matrix (got %v)", c.Matrix))
	expect(len(c.Empty) == 0, "empty list")
	expect(len(c.ByName) == 3 && c.ByName["one"] == 1 && c.ByName["two"] == 2 && c.ByName["neg"] == -3,
		fmt.Sprintf("by_name (got %v)", c.ByName))
	expect(len(c.ById) == 2, "by_id has two entries")
	expect(sameScalars(c.ById[-1], canonicalScalars()), "by_id[-1] is the canonical scalars")
	unflagged := canonicalScalars()
	unflagged.Flag = false
	expect(sameScalars(c.ById[42], unflagged), "by_id[42] is the canonical scalars with flag false")
	expect(sameScalars(c.Scalars, canonicalScalars()), "nested scalars")
	expect(sameShape(c.Shape, wv.ShapeLabeled{Label: "tag", Count: 3}), fmt.Sprintf("shape (got %+v)", c.Shape))
	expect(len(c.Shapes) == 5, fmt.Sprintf("shapes has one of each variant (got %d)", len(c.Shapes)))
	expect(sameShape(c.Shapes[0], wv.ShapeEmpty{}), "shapes[0] Empty")
	expect(sameShape(c.Shapes[1], wv.ShapeCircle{Radius: 2.5}), "shapes[1] Circle")
	expect(sameShape(c.Shapes[2], wv.ShapeRect{Width: 1.0, Height: 0.5}), "shapes[2] Rect")
	expect(sameShape(c.Shapes[3], wv.ShapeLabeled{Label: "", Count: -1}), "shapes[3] Labeled")
	expect(sameShape(c.Shapes[4], wv.ShapeNested{Inner: canonicalScalars(), Note: ptrStr("n")}), "shapes[4] Nested")
	expect(sameShape(c.MaybeShape, wv.ShapeNested{Inner: canonicalScalars(), Note: nil}), "maybe_shape Nested without note")
	expect(sameBytes(c.MaybeList, []byte{9, 8}), fmt.Sprintf("maybe_list (got %v)", c.MaybeList))
	expect(len(c.Sparse) == 3 && c.Sparse[0] != nil && *c.Sparse[0] && c.Sparse[1] == nil &&
		c.Sparse[2] != nil && !*c.Sparse[2], "sparse [true, nil, false]")
	expect(len(c.Colors) == 3 && c.Colors[0] == wv.ColorRed && c.Colors[1] == wv.ColorGreen && c.Colors[2] == wv.ColorBlue,
		fmt.Sprintf("colors (got %v)", c.Colors))

	// Go encodes, producer decodes and compares; then echo and compare.
	ok, err = wv.VerifyComposite(c)
	expect(err == nil && ok, fmt.Sprintf("verify_composite(sample) (err %v)", err))
	expectSameComposite(wv.RoundtripComposite(c), c, "roundtrip_composite(sample)")
	desc := wv.DescribeComposite(c)
	expect(strings.Contains(desc, "name: \"héllo wörld ✓\"") && strings.Contains(desc, "some_i64: Some(-9223372036854775808)"),
		fmt.Sprintf("describe_composite renders the sample (got %s)", desc))

	// A one-field change is a mismatch.
	c2 := wv.RoundtripComposite(c)
	c2.Sparse[1] = ptrBool(true)
	ok, err = wv.VerifyComposite(c2)
	cerr = nil
	expect(!ok && errors.As(err, &cerr) && cerr.Code == wv.CodecErrorMismatch, "verify_composite(changed) is Mismatch")

	// A Composite built from scratch with edge values.
	custom := wv.Composite{
		Name:     "a\x00b \U0001F600 ünï",
		Blob:     []byte{},
		SomeI64:  ptrI64(math.MaxInt64),
		NoneI64:  ptrI64(math.MinInt64),
		SomeText: nil,
		Names:    []string{},
		Matrix:   [][]int32{{}, {math.MinInt32, math.MaxInt32}},
		Empty:    []float64{math.NaN(), math.Inf(1), math.Inf(-1), math.Copysign(0, -1), 5e-324},
		ByName:   map[string]int64{"": 0, "k": math.MinInt64, "z": math.MaxInt64},
		ById:     map[int32]wv.Scalars{math.MinInt32: extreme, math.MaxInt32: canonicalScalars(), 0: {}},
		Scalars:  wv.Scalars{},
		Shape:    wv.ShapeEmpty{},
		Shapes:   []wv.Shape{},
		MaybeShape: wv.ShapeNested{
			Inner: extreme,
			Note:  ptrStr(""),
		},
		MaybeList: []byte{},
		Sparse:    []*bool{nil, nil},
		Colors:    []wv.Color{},
	}
	echoed := wv.RoundtripComposite(custom)
	expectSameComposite(echoed, custom, "roundtrip_composite(custom)")
	expect(echoed.MaybeList != nil && len(echoed.MaybeList) == 0, "present-but-empty optional list stays present")
	expect(echoed.SomeText == nil, "absent optional string stays absent")
	expect(math.IsNaN(echoed.Empty[0]) && math.IsInf(echoed.Empty[1], 1) && math.IsInf(echoed.Empty[2], -1) &&
		math.Signbit(echoed.Empty[3]) && echoed.Empty[4] == 5e-324, "float specials survive inside a list")
	ok, err = wv.VerifyComposite(custom)
	cerr = nil
	expect(!ok && errors.As(err, &cerr), "the custom composite is not the fixture")
	desc = wv.DescribeComposite(custom)
	expect(strings.Contains(desc, "name: \"a\\0b 😀 ünï\"") && strings.Contains(desc, "empty: [NaN, inf, -inf, -0.0, 5e-324]"),
		fmt.Sprintf("describe_composite renders the custom value (got %s)", desc))

	// An absent optional list and absent optional shape.
	custom.MaybeList = nil
	custom.MaybeShape = nil
	echoed = wv.RoundtripComposite(custom)
	expect(echoed.MaybeList == nil && echoed.MaybeShape == nil, "absent optionals stay absent")

	// ── Shape: every variant, alone and in a list ──
	shapes := []wv.Shape{
		wv.ShapeEmpty{},
		wv.ShapeCircle{Radius: math.Inf(1)},
		wv.ShapeRect{Width: -0.0, Height: float32(math.Inf(-1))},
		wv.ShapeLabeled{Label: "✓ label", Count: math.MinInt32},
		wv.ShapeNested{Inner: extreme, Note: nil},
		wv.ShapeNested{Inner: canonicalScalars(), Note: ptrStr("note")},
	}
	for i, sh := range shapes {
		expect(sameShape(wv.RoundtripShape(sh), sh), fmt.Sprintf("roundtrip_shape[%d] (%+v)", i, sh))
	}
	echoedShapes := wv.RoundtripShapes(shapes)
	expect(len(echoedShapes) == len(shapes), "roundtrip_shapes length")
	for i := range shapes {
		expect(sameShape(echoedShapes[i], shapes[i]), fmt.Sprintf("roundtrip_shapes[%d]", i))
	}
	expect(len(wv.RoundtripShapes(nil)) == 0, "roundtrip_shapes of nothing")
	expect(wv.DescribeShape(wv.ShapeEmpty{}) == "Empty", "describe_shape(Empty)")
	expect(wv.DescribeShape(wv.ShapeCircle{Radius: 2.5}) == "Circle { radius: 2.5 }",
		fmt.Sprintf("describe_shape(Circle) (got %q)", wv.DescribeShape(wv.ShapeCircle{Radius: 2.5})))
	expect(wv.DescribeShape(wv.ShapeLabeled{Label: "tag", Count: 3}) == "Labeled { label: \"tag\", count: 3 }",
		"describe_shape(Labeled)")
	switch v := wv.RoundtripShape(wv.ShapeRect{Width: 1, Height: 2}).(type) {
	case wv.ShapeRect:
		expect(v.Width == 1 && v.Height == 2, "Rect payload")
	default:
		expect(false, fmt.Sprintf("roundtrip_shape(Rect) returned %T", v))
	}

	// ── Top-level buffered and direct echoes ──
	expect(wv.RoundtripOptI64(nil) == nil, "roundtrip_opt_i64(nil)")
	o := wv.RoundtripOptI64(ptrI64(math.MinInt64))
	expect(o != nil && *o == math.MinInt64, "roundtrip_opt_i64(MinInt64)")
	o = wv.RoundtripOptI64(ptrI64(0))
	expect(o != nil && *o == 0, "roundtrip_opt_i64(0) is present")
	m := wv.RoundtripMap(map[string]int64{"": math.MaxInt64, "héllo": -1, "x": 0})
	expect(len(m) == 3 && m[""] == math.MaxInt64 && m["héllo"] == -1 && m["x"] == 0, fmt.Sprintf("roundtrip_map (got %v)", m))
	expect(len(wv.RoundtripMap(nil)) == 0, "roundtrip_map(empty)")
	expect(wv.RoundtripString("héllo wörld ✓ 😀") == "héllo wörld ✓ 😀", "roundtrip_string unicode")
	expect(wv.RoundtripString("") == "", "roundtrip_string empty")
	expect(sameBytes(wv.RoundtripBytes([]byte{0, 255, 128}), []byte{0, 255, 128}), "roundtrip_bytes")
	expect(len(wv.RoundtripBytes([]byte{})) == 0, "roundtrip_bytes empty")
	expect(wv.RoundtripI64(math.MinInt64) == math.MinInt64, "roundtrip_i64 MinInt64")
	expect(wv.RoundtripI64(math.MaxInt64) == math.MaxInt64, "roundtrip_i64 MaxInt64")
	expect(wv.RoundtripU64(math.MaxUint64) == math.MaxUint64, "roundtrip_u64 MaxUint64")
	expect(wv.RoundtripU64(1<<63) == 1<<63, "roundtrip_u64 2^63")
	expect(math.IsNaN(wv.RoundtripF64(math.NaN())), "roundtrip_f64 NaN")
	expect(math.IsInf(wv.RoundtripF64(math.Inf(1)), 1) && math.IsInf(wv.RoundtripF64(math.Inf(-1)), -1), "roundtrip_f64 infinities")
	negZero := wv.RoundtripF64(math.Copysign(0, -1))
	expect(negZero == 0 && math.Signbit(negZero), "roundtrip_f64 -0")
	expect(wv.RoundtripF64(5e-324) == 5e-324 && wv.RoundtripF64(math.MaxFloat64) == math.MaxFloat64, "roundtrip_f64 subnormal and max")
	expect(wv.RoundtripBool(true) && !wv.RoundtripBool(false), "roundtrip_bool")
	expect(wv.RoundtripColor(wv.ColorBlue) == wv.ColorBlue && wv.RoundtripColor(wv.ColorGreen) == 1, "roundtrip_color")

	// ── Holder: objects inside buffers ──
	h := wv.MakeHolder(10, true)
	expect(h.Primary != nil && h.Primary.Value() == 10, "holder.primary value 10")
	expect(h.Spare != nil && h.Spare.Value() == 11, "holder.spare value 11")
	expect(len(h.Many) == 3 && h.Many[0].Value() == 12 && h.Many[1].Value() == 13 && h.Many[2].Value() == 14,
		"holder.many values 12..14")
	// Each encoding mints fresh references; the producer adopts and drops
	// them, so repeated calls keep the wrappers valid.
	expect(wv.SumHolder(h) == 10+11+12+13+14, "sum_holder(holder)")
	expect(wv.SumHolder(h) == 60, "sum_holder is repeatable")
	expect(h.Primary.Value() == 10, "wrapper alive after being encoded twice")

	// primary_of returns a wrapper to the SAME object as holder.primary.
	p := wv.PrimaryOf(h)
	expect(p != nil && p != h.Primary, "primary_of yields a distinct wrapper")
	expect(p.Value() == 10, "primary_of value 10")
	expect(wv.SamePrimary(h, wv.Holder{Primary: p}), "same_primary(holder, {primary_of}) is true")
	expect(wv.SamePrimary(h, h), "same_primary(holder, holder) is true")
	expect(!wv.SamePrimary(h, wv.Holder{Primary: h.Spare}), "same_primary against the spare is false")
	other := wv.MakeHolder(10, true)
	expect(!wv.SamePrimary(h, other), "equal values, distinct objects")

	// Without a spare.
	bare := wv.MakeHolder(0, false)
	expect(bare.Spare == nil, "make_holder(0, false) has no spare")
	expect(wv.SumHolder(bare) == 0+2+3+4, "sum_holder without spare")

	// A holder built from Go-constructed tokens.
	mine := wv.Holder{
		Primary: wv.NewToken(-5),
		Spare:   nil,
		Many:    []*wv.Token{wv.NewToken(1), wv.NewToken(2)},
	}
	expect(mine.Primary.Value() == -5, "new token value")
	expect(wv.SumHolder(mine) == -2, "sum_holder(mine)")
	mp := wv.PrimaryOf(mine)
	expect(mp.Value() == -5 && wv.SamePrimary(mine, wv.Holder{Primary: mp}), "primary_of(mine) aliases the Go-made token")
	expect(wv.SumHolder(wv.Holder{Primary: mp, Spare: mp, Many: []*wv.Token{mp, mp}}) == -20,
		"the same wrapper may be encoded in several positions at once")
	big := wv.NewToken(math.MaxInt64)
	expect(big.Value() == math.MaxInt64 && wv.SumHolder(wv.Holder{Primary: big}) == math.MaxInt64, "token carries MaxInt64")
	big.Close()

	// Release: closing the holder's wrapper leaves primary_of's reference
	// alive; every Close is idempotent; a closed wrapper traps on use and
	// can't be encoded.
	h.Primary.Close()
	h.Primary.Close()
	expect(p.Value() == 10, "primary_of's reference survives closing holder.primary")
	expect(catchPanic(func() { h.Primary.Value() }) != nil, "use after Close traps")
	expect(catchPanic(func() { wv.SumHolder(h) }) != nil, "encoding a closed token traps")
	p.Close()
	p.Close()
	h.Spare.Close()
	for _, t := range h.Many {
		t.Close()
		t.Close()
	}
	other.Primary.Close()
	other.Spare.Close()
	for _, t := range other.Many {
		t.Close()
	}
	bare.Primary.Close()
	for _, t := range bare.Many {
		t.Close()
	}
	mine.Primary.Close()
	for _, t := range mine.Many {
		t.Close()
	}
	mp.Close()
	mp.Close()

	fmt.Println("go/codec: OK")
}
