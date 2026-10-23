// Conformance consumer: calculator sample, Kotlin (JVM via JNI) target.
//
// The getting-started surface through the generated `Calculator` object: a
// direct-value call (wrapping at the `Int` edges), a throwing call that
// raises the typed `CalcException.DivisionByZero`, a string in and out
// (non-ASCII and astral text and an interior NUL survive), a second error
// domain whose code value collides with the first's (`ParseException`),
// an untyped `throws: any` failure (`FfiException` code -1), an optional
// scalar return (`Double?`), and typed arrays in and out. Open domains: an
// unknown positive code maps to the domain's base exception. Loading the
// bindings checks the ABI revision and the `calculator` contract (see
// load_failure.kt for a failed load). Ends by asserting the producer's leak
// counters are zero.
@file:JvmName("Main")

import calculator.CalcException
import calculator.Calculator
import calculator.FfiException
import calculator.JniBridge
import calculator.NativeLibrary
import calculator.ParseException

fun main() {
    NativeLibrary.load()
    NativeLibrary.load() // loading twice is a no-op
    expect(JniBridge.debug_live(-1) == 1L, "the sample counts live allocations")

    expect(Calculator.add(2, 3) == 5, "add(2, 3)")
    expect(Calculator.add(-7, 7) == 0, "add(-7, 7)")
    expect(Calculator.add(Int.MAX_VALUE, 1) == Int.MIN_VALUE, "add wraps")

    expect(Calculator.divide(10, 2) == 5, "divide(10, 2)")
    expect(Calculator.divide(-7, 2) == -3, "divide rounds toward zero")
    expect(Calculator.divide(Int.MIN_VALUE, -1) == Int.MIN_VALUE, "divide(MIN, -1)")

    val err = thrownBy { Calculator.divide(1, 0) }
    expect(err is CalcException.DivisionByZero, "divide(1, 0) raises DivisionByZero (got $err)")
    expect(err is FfiException && err.code == 1, "DivisionByZero has code 1")
    expect(err?.message == "division by zero", "DivisionByZero message (got ${err?.message})")

    expect(Calculator.greet("World") == "Hello, World!", "greet(World)")
    expect(Calculator.greet("") == "Hello, !", "greet(empty)")
    expect(Calculator.greet("Wörld 🦀") == "Hello, Wörld 🦀!", "greet keeps non-ASCII and astral text")
    expect(Calculator.greet("a\u0000b") == "Hello, a\u0000b!", "greet keeps an interior NUL")

    // A second domain: its code 1 is NotANumber, not DivisionByZero.
    expect(Calculator.parse("42") == 42 && Calculator.parse(" 42 ") == 42, "parse(42)")
    val nan = thrownBy { Calculator.parse("4x") }
    expect(nan is ParseException.NotANumber, "parse(4x) raises NotANumber (got $nan)")
    nan as ParseException.NotANumber
    expect(nan.code == 1 && nan.text == "4x" && nan.message == "not a number: 4x", "NotANumber payload and message")
    val empty = thrownBy { Calculator.parse("") }
    expect(
        empty is ParseException.NotANumber && empty.text == "" && empty.message == "not a number: ",
        "parse(\"\") raises NotANumber with empty text (got $empty)",
    )

    // throws: any
    expect(Calculator.sqrt(9.0) == 3.0, "sqrt(9)")
    val neg = thrownBy { Calculator.sqrt(-4.0) }
    expect(
        neg is FfiException && neg !is CalcException && neg !is ParseException && neg.code == -1,
        "sqrt(-4) raises an untyped FfiException with code -1 (got $neg)",
    )
    expect(neg?.message == "cannot take the square root of -4", "sqrt(-4) message (got ${neg?.message})")

    // An optional scalar return.
    expect(Calculator.mean(listOf(1.0, 2.0, 6.0)) == 3.0, "mean([1, 2, 6])")
    expect(Calculator.mean(emptyList()) == null, "mean([]) is absent")

    // Typed arrays in and out.
    expect(Calculator.runningTotal(listOf(1, 2, 3)) == listOf(1, 3, 6), "runningTotal([1, 2, 3])")
    expect(Calculator.runningTotal(emptyList()).isEmpty(), "runningTotal([])")
    expect(
        Calculator.runningTotal(listOf(1, 2, 3, Int.MAX_VALUE)) == listOf(1, 3, 6, -2147483643),
        "runningTotal wraps",
    )
    val big = List(1000) { it }
    expect(Calculator.runningTotal(big).last() == 499500, "runningTotal of a heap-copied array")

    // Open domains: a code these bindings don't know maps to the domain's
    // base exception with its code and message (domains 2 and 3 are
    // CalcError and ParseError, in declaration order).
    val unknown = JniBridge.error(2, 77, "from the future".toByteArray(), null)
    expect(
        unknown is CalcException && unknown !is CalcException.DivisionByZero && unknown.code == 77,
        "an unknown CalcError code is a plain CalcException (got $unknown)",
    )
    expect(unknown.message == "from the future", "the unknown code keeps its message")
    val unknownParse = JniBridge.error(3, 1, "no payload".toByteArray(), null)
    expect(
        unknownParse is ParseException && unknownParse !is ParseException.NotANumber,
        "a payload-less NotANumber falls back to the base ParseException (got $unknownParse)",
    )

    expectNoLeaks(JniBridge::debug_live)
    println("kotlin/calculator: OK")
}
