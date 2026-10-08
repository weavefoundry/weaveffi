"""Conformance consumer: calculator sample, Python target (ABI revision 4).

The getting-started surface: direct `i32` arguments and returns (with
wrapping), a throwing call raising the typed `CalcError.DivisionByZero`, and
a string round trip (non-ASCII and astral text and an interior NUL survive,
since strings cross as pointer + length). Importing the package already
checked the ABI revision and the `calculator` contract table. Ends with the
leak check (see harness.py).
"""
import calculator as calc
from harness import Consumer

INT32_MIN = -(2**31)
INT32_MAX = 2**31 - 1

consumer = Consumer("calculator", calc)
check = consumer.check


def main() -> None:
    check(calc.add(2, 3) == 5, "add")
    check(calc.add(-7, 7) == 0, "add to zero")
    check(calc.add(INT32_MAX, 1) == INT32_MIN, "add wraps")

    check(calc.divide(10, 2) == 5, "divide")
    check(calc.divide(-7, 2) == -3, "divide rounds toward zero")
    check(calc.divide(INT32_MIN, -1) == INT32_MIN, "divide MIN by -1")

    exc = consumer.raises(calc.CalcError.DivisionByZero, lambda: calc.divide(1, 0), "divide(1, 0)")
    check(exc.code == 1 and exc.CODE == 1, f"DivisionByZero code {exc.code}")
    check(exc.message == "division by zero", f"DivisionByZero message {exc.message!r}")
    check(isinstance(exc, calc.CalcError) and isinstance(exc, calc.Error), "error hierarchy")
    check(calc.DivisionByZero is calc.CalcError.DivisionByZero, "scoped alias")

    for name, expected in [
        ("World", "Hello, World!"),
        ("", "Hello, !"),
        ("Wörld 🦀", "Hello, Wörld 🦀!"),
        ("a\0b", "Hello, a\0b!"),
    ]:
        got = calc.greet(name)
        check(got == expected, f"greet({name!r}) == {got!r}")

    consumer.finish()


main()
