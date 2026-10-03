"""Conformance consumer: calculator sample, Python target.

The smallest surface: direct `i32` arguments and returns, a throwing call
raising the typed `CalcError.DivisionByZero`, and a string round trip
(including an interior NUL and non-ASCII text, since strings cross as
pointer + length). Ends with the leak check (see harness.py).
"""
import calculator as wv
from harness import Consumer

consumer = Consumer("calculator")
check = consumer.check


def main() -> None:
    check(wv.add(2, 3) == 5, "add")
    check(wv.mul(-4, 6) == -24, "mul")
    check(wv.div(17, 5) == 3, "div")
    try:
        wv.div(1, 0)
        check(False, "expected DivisionByZero")
    except wv.CalcError.DivisionByZero as exc:
        check(exc.code == 1 and isinstance(exc, wv.CalcError) and isinstance(exc, wv.Error),
              f"DivisionByZero {exc!r}")
        check(exc.message == "division by zero", f"message {exc.message!r}")
    for s in ["", "hello", "nul\0inside", "\0", "héllo wörld ✓"]:
        check(wv.echo(s) == s, f"echo {s!r}")
    consumer.finish(wv)


main()
