"""Conformance consumer: calculator sample, Python target (ABI revision 5).

The getting-started surface: direct `i32` arguments and returns (with
wrapping, and range-checked before ctypes could truncate them), two error
domains in one module whose codes share a value (`CalcError.DivisionByZero`
and `ParseError.NotANumber` are both 1, told apart by the callable's
domain), a `throws: any` function raising the root error with code -1, a
string round trip (non-ASCII and astral text and an interior NUL survive,
since strings cross as pointer + length), an optional `f64` return (crossing
as a flag plus a value), and typed arrays in both directions (any sequence
of numbers in; a list out). Importing the package already checked the ABI
revision and the `calculator` contract table. Ends with the leak check (see
harness.py).
"""
import array
import ctypes

import calculator as calc
from harness import Consumer

INT32_MIN = -(2**31)
INT32_MAX = 2**31 - 1

consumer = Consumer("calculator", calc)
check = consumer.check
raises = consumer.raises
impl = consumer.impl


def arithmetic() -> None:
    check(calc.add(2, 3) == 5, "add")
    check(calc.add(-7, 7) == 0, "add to zero")
    check(calc.add(INT32_MAX, 1) == INT32_MIN, "add wraps")

    check(calc.divide(10, 2) == 5, "divide")
    check(calc.divide(-7, 2) == -3, "divide rounds toward zero")
    check(calc.divide(INT32_MIN, -1) == INT32_MIN, "divide MIN by -1")

    exc = raises(calc.CalcError.DivisionByZero, lambda: calc.divide(1, 0), "divide(1, 0)")
    check(type(exc) is calc.DivisionByZeroError, f"DivisionByZero class {exc!r}")
    check(exc.code == 1 and exc.CODE == 1, f"DivisionByZero code {exc.code}")
    check(exc.message == "division by zero", f"DivisionByZero message {exc.message!r}")
    check(isinstance(exc, calc.CalcError) and isinstance(exc, calc.Error), "error hierarchy")
    check(calc.DivisionByZeroError is calc.CalcError.DivisionByZero, "scoped alias")

    # ctypes would truncate these silently; the bindings refuse them first.
    exc2 = raises(OverflowError, lambda: calc.add(INT32_MAX + 1, 0), "add(2^31, 0)")
    check(str(exc2) == "a: 2147483648 is out of range for i32", str(exc2))
    raises(OverflowError, lambda: calc.add(0, INT32_MIN - 1), "add(0, -2^31 - 1)")
    exc3 = raises(TypeError, lambda: calc.add(1.5, 2), "add(1.5, 2)")  # type: ignore[arg-type]
    check(str(exc3) == "a: expected an integer, got float", str(exc3))


def strings() -> None:
    for name, expected in [
        ("World", "Hello, World!"),
        ("", "Hello, !"),
        ("Wörld 🦀", "Hello, Wörld 🦀!"),
        ("a\0b", "Hello, a\0b!"),
    ]:
        got = calc.greet(name)
        check(got == expected, f"greet({name!r}) == {got!r}")


def domains() -> None:
    check(calc.parse("42") == 42 and calc.parse(" 42 ") == 42, "parse")
    for text in ["4x", ""]:
        exc = raises(calc.ParseError.NotANumber, lambda: calc.parse(text), f"parse({text!r})")
        check(type(exc) is calc.NotANumberError, f"NotANumber class {exc!r}")
        check(exc.code == 1 and exc.text == text, f"NotANumber payload {exc!r}")
        check(exc.message == f"not a number: {text}", f"NotANumber message {exc.message!r}")
        # Code 1 of the other domain is not this one.
        check(not isinstance(exc, calc.CalcError), "dispatched by the callable's domain")

    check(calc.sqrt(9.0) == 3.0, "sqrt(9)")
    exc = raises(calc.Error, lambda: calc.sqrt(-4.0), "sqrt(-4)")
    check(type(exc) is calc.Error, f"throws any raises the root error {exc!r}")
    check(exc.code == calc.Error.GENERIC_ERROR_CODE == -1, f"sqrt code {exc.code}")
    check(exc.message == "cannot take the square root of -4", f"sqrt message {exc.message!r}")

    # A domain is open: a code a newer library added maps to its base class.
    unknown = impl._calc_error_from(77, "from the future", b"")
    check(type(unknown) is calc.CalcError and unknown.code == 77
          and unknown.message == "from the future", f"unknown code {unknown!r}")
    runtime = impl._parse_error_from(-2, "panicked", b"")
    check(type(runtime) is calc.Error and runtime.code == -2, f"runtime code {runtime!r}")


def slices() -> None:
    check(calc.mean([1.0, 2.0, 6.0]) == 3.0, "mean present")
    check(calc.mean([]) is None, "mean absent")
    check(calc.mean((1, 2)) == 1.5, "mean of a tuple of ints")
    check(calc.mean(array.array("d", [2.0, 4.0])) == 3.0, "mean of an array lent as is")
    check(calc.mean(x / 2 for x in range(1, 4)) == 1.0, "mean of a generator")  # type: ignore[arg-type]

    check(calc.running_total([1, 2, 3]) == [1, 3, 6], "running_total")
    check(calc.running_total([]) == [], "running_total empty")
    check(calc.running_total([1, 2, 3, INT32_MAX]) == [1, 3, 6, -2147483643],
          "running_total wraps")
    check(calc.running_total(range(4)) == [0, 1, 3, 6], "running_total of a range")

    exc = raises(OverflowError, lambda: calc.running_total([1, INT32_MAX + 1]), "element 2^31")
    check(str(exc) == "values: an element is out of range for i32", str(exc))
    exc2 = raises(TypeError, lambda: calc.running_total([1, "2"]),  # type: ignore[list-item]
                  "string element")
    check(str(exc2).startswith("values: "), str(exc2))
    raises(TypeError, lambda: calc.running_total(b"\x01\x02"), "bytes")
    raises(TypeError, lambda: calc.mean("12"), "str")  # type: ignore[arg-type]

    # A typed array that isn't aligned for its element type is -3.
    raw = (ctypes.c_uint8 * 16)()
    has = ctypes.c_double()
    err = impl._ErrorStruct()
    present = impl._c_calculator_mean(ctypes.addressof(raw) + 1, 1, ctypes.byref(has),
                                      ctypes.byref(err))
    code, message, _ = impl._read_error(err)
    check(not present and code == -3, f"misaligned array: {code} {message!r}")


def main() -> None:
    arithmetic()
    strings()
    domains()
    slices()
    consumer.finish()


main()
