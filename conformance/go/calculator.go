// Conformance consumer: calculator sample, Go target (ABI revision 4).
//
// The getting-started surface: a direct-value call (wrapping on overflow),
// a throwing call that returns the typed *DivisionByZeroError, and a string
// in and out (non-ASCII and astral text and an interior NUL byte survive).
// Importing the bindings checks the ABI revision and the contract table.
// Ends by asserting the library's leak counters are zero.

package main

import (
	"fmt"
	"math"

	calc "__MODPATH__"
)

func main() {
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

	expectNoLeaks(calc.DebugLive)
	fmt.Println("go/calculator: OK")
}
