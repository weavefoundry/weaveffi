// Conformance consumer: calculator sample, Kotlin (JVM via JNI) target.
//
// The getting-started surface through the generated `Calculator` object: a
// direct-value call (wrapping at the `Int` edges), a throwing call that
// raises the typed `CalcException.DivisionByZero`, and a string in and out
// (non-ASCII and astral text and an interior NUL survive). Loading the
// bindings checks the ABI revision and the `calculator` contract. Ends by
// asserting the producer's leak counters are zero.
@file:JvmName("Main")

import calculator.CalcException
import calculator.Calculator
import calculator.FfiException
import calculator.JniBridge

fun main() {
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

    expectNoLeaks(JniBridge::debug_live)
    println("kotlin/calculator: OK")
}
