// Conformance check: a failed load is a catchable `LinkageError`, Kotlin
// (JVM via JNI) target, calculator sample.
//
// Run with `CALCULATOR_LIBRARY` naming a library that doesn't exist.
// `NativeLibrary.load()` throws `UnsatisfiedLinkError` with the reason, and
// the same again on every later call; the first binding call throws that
// error too, and every later one `NoClassDefFoundError`. Both are
// `LinkageError`s, so an app can catch the failure and carry on.
@file:JvmName("LoadFailure")

import calculator.Calculator
import calculator.NativeLibrary

fun main() {
    val first = thrownBy { NativeLibrary.load() }
    expect(first is UnsatisfiedLinkError, "load() raises UnsatisfiedLinkError (got $first)")
    expect(first?.message?.contains("does-not-exist") == true, "the message names the library (got ${first?.message})")
    val second = thrownBy { NativeLibrary.load() }
    expect(
        second is UnsatisfiedLinkError && second.message == first?.message,
        "load() raises the same error again (got $second)",
    )
    val call = thrownBy { Calculator.add(1, 2) }
    expect(call is UnsatisfiedLinkError, "the first call raises UnsatisfiedLinkError (got $call)")
    val again = thrownBy { Calculator.add(1, 2) }
    expect(again is LinkageError, "a later call raises a LinkageError (got $again)")
    println("kotlin/calculator load failure: OK")
}
