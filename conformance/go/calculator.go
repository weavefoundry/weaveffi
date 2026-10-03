// Conformance consumer: calculator sample, Go target.
//
// The minimal surface: plain scalar functions, the throwing Div with its
// typed *CalcError, and a string round trip that keeps interior NUL bytes and
// non-ASCII text intact through the (pointer, length) string ABI. Exits 0 on
// success; aborts (non-zero) on any mismatch.

package main

import (
	"errors"
	"fmt"

	wv "__MODPATH__"
)

func main() {
	expect(wv.Add(2, 3) == 5, "add")
	expect(wv.Mul(-4, 6) == -24, "mul")

	q, err := wv.Div(17, 5)
	expect(err == nil && q == 3, fmt.Sprintf("div (got %d, %v)", q, err))
	_, err = wv.Div(1, 0)
	var cerr *wv.CalcError
	expect(errors.As(err, &cerr), fmt.Sprintf("div by zero yields *CalcError (got %T %v)", err, err))
	expect(cerr.Code == wv.CalcErrorDivisionByZero, "division-by-zero code")
	expect(cerr.Message == "division by zero", fmt.Sprintf("message (got %q)", cerr.Message))

	for _, s := range []string{"", "hello", "a\x00b\x00", "héllo ✓ 😀"} {
		expect(wv.Echo(s) == s, fmt.Sprintf("echo %q", s))
	}

	expectNoLeaks(wv.DebugLive)
	fmt.Println("go/calculator: OK")
}
