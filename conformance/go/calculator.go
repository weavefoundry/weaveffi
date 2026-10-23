// Conformance consumer: calculator sample, Go target (ABI revision 5).
//
// The getting-started surface: Check (the ABI revision and the contract
// table), a direct-value call (wrapping on overflow), a throwing call that
// returns the typed *DivisionByZeroError, and a string in and out
// (non-ASCII and astral text and an interior NUL byte survive). Then the
// ABI 5 shapes: a second error domain whose code value 1 is also CalcError's
// (dispatched by the function's domain), a payload field, a function that
// fails with any error (*Error, code -1), an optional scalar return, and
// typed arrays in and out. Ends by asserting the library's leak counters are
// zero.

package main

import (
	"errors"
	"fmt"
	"math"
	"slices"

	calc "__MODPATH__"
)

func main() {
	expect(calc.Check() == nil, fmt.Sprintf("Check() = %v", calc.Check()))

	expect(calc.Add(2, 3) == 5, "add(2, 3)")
	expect(calc.Add(-7, 7) == 0, "add(-7, 7)")
	expect(calc.Add(math.MaxInt32, 1) == math.MinInt32, "add wraps")

	for _, c := range []struct{ a, b, q int32 }{
		{10, 2, 5},
		{-7, 2, -3}, // rounds toward zero
		{math.MinInt32, -1, math.MinInt32},
	} {
		q, err := calc.Divide(c.a, c.b)
		expect(err == nil && q == c.q, fmt.Sprintf("divide(%d, %d) = %d, %v", c.a, c.b, q, err))
	}

	q, err := calc.Divide(1, 0)
	expect(q == 0, "a failed divide returns zero")
	e := expectAs[*calc.DivisionByZeroError](err, "divide(1, 0)")
	expect(e.Code() == 1 && e.Error() == "division by zero", fmt.Sprintf("DivisionByZero code and message (got %d %q)", e.Code(), e.Error()))
	expectAs[calc.CalcError](err, "divide(1, 0) is a CalcError")

	for name, want := range map[string]string{
		"World":    "Hello, World!",
		"":         "Hello, !",
		"Wörld 🦀":  "Hello, Wörld 🦀!",
		"a\x00b":   "Hello, a\x00b!",
		"😀 astral": "Hello, 😀 astral!",
	} {
		got := calc.Greet(name)
		expect(got == want, fmt.Sprintf("greet(%q) = %q", name, got))
	}

	abi5()

	expectNoLeaks(calc.DebugLive)
	fmt.Println("go/calculator: OK")
}

func abi5() {
	// A second domain: ParseError's code 1 is not CalcError's code 1.
	for _, text := range []string{"42", " 42 "} {
		n, err := calc.Parse(text)
		expect(err == nil && n == 42, fmt.Sprintf("parse(%q) = %d, %v", text, n, err))
	}
	for _, text := range []string{"4x", ""} {
		_, err := calc.Parse(text)
		e := expectAs[*calc.NotANumberError](err, fmt.Sprintf("parse(%q)", text))
		expect(e.Code() == 1 && e.Text == text, fmt.Sprintf("NotANumber code %d text %q", e.Code(), e.Text))
		expect(e.Error() == "not a number: "+text, "NotANumber message: "+e.Error())
		expectAs[calc.ParseError](err, "a NotANumberError is a ParseError")
		var calcErr calc.CalcError
		expect(!errors.As(err, &calcErr), "a ParseError isn't a CalcError")
	}

	// A function that fails with any error returns an *Error with code -1.
	root, err := calc.Sqrt(9)
	expect(err == nil && root == 3, fmt.Sprintf("sqrt(9) = %v, %v", root, err))
	_, err = calc.Sqrt(-4)
	e := expectAs[*calc.Error](err, "sqrt(-4)")
	expect(e.Code == -1 && e.Message == "cannot take the square root of -4", fmt.Sprintf("sqrt(-4): %d %q", e.Code, e.Message))

	// An optional scalar return from a typed-array parameter.
	m := calc.Mean([]float64{1, 2, 6})
	expect(m != nil && *m == 3, "mean([1, 2, 6])")
	expect(calc.Mean(nil) == nil && calc.Mean([]float64{}) == nil, "mean([]) is absent")

	// Typed arrays in both directions.
	expect(slices.Equal(calc.RunningTotal([]int32{1, 2, 3}), []int32{1, 3, 6}), "running_total([1, 2, 3])")
	empty := calc.RunningTotal(nil)
	expect(empty != nil && len(empty) == 0, "running_total([]) is an empty slice")
	wrapped := calc.RunningTotal([]int32{1, 2, 3, math.MaxInt32})
	expect(slices.Equal(wrapped, []int32{1, 3, 6, -2147483643}), fmt.Sprintf("running_total wraps (got %v)", wrapped))
	// The input is lent, not copied or changed.
	in := []int32{5, 5}
	expect(slices.Equal(calc.RunningTotal(in), []int32{5, 10}) && slices.Equal(in, []int32{5, 5}), "the input is unchanged")
}
