// Conformance consumer: calculator sample, Swift target (ABI revision 4).
//
// The getting-started surface through the generated `Calculator` module: a
// direct-value call (wrapping on overflow), a throwing call whose failure is
// the typed `CalcError.divisionByZero`, and a string in and out (non-ASCII
// text, an astral character, and an interior NUL survive). The bindings check
// the ABI revision and the `calculator` contract table before the first call.
// Ends by asserting the producer's leak counters are zero. Exits non-zero on
// any mismatch.

import CCalculator
import Calculator
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: @autoclosure () -> String) {
    if !cond { fail(msg()) }
}

/// Every live-resource counter must settle at zero: 0 objects, 1 callbacks,
/// 2 iterators, 3 cancel tokens, 4 allocations.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<2000 {
        live = (0..<5).map { calculator_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(1000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

func greet(_ name: String, _ expected: String) {
    let got = Calculator.greet(name: name)
    expect(Array(got.utf8) == Array(expected.utf8), "greet(\(name.debugDescription)) == \(got.debugDescription)")
}

func run() {
    expect(Calculator.add(a: 2, b: 3) == 5, "add(2, 3)")
    expect(Calculator.add(a: -7, b: 7) == 0, "add(-7, 7)")
    expect(Calculator.add(a: .max, b: 1) == .min, "add wraps")

    do {
        expect(try Calculator.divide(a: 10, b: 2) == 5, "divide(10, 2)")
        expect(try Calculator.divide(a: -7, b: 2) == -3, "divide rounds toward zero")
        expect(try Calculator.divide(a: .min, b: -1) == .min, "divide(MIN, -1)")
    } catch {
        fail("divide threw \(error)")
    }

    do {
        _ = try Calculator.divide(a: 1, b: 0)
        fail("divide(1, 0) returned")
    } catch let error as CalcError {
        guard case let .divisionByZero(message) = error else { fail("unexpected \(error)") }
        expect(error.errorCode == 1, "DivisionByZero code (got \(error.errorCode))")
        expect(message == "division by zero", "DivisionByZero message (got \(message))")
        expect(error.localizedDescription == "division by zero", "localizedDescription")
    } catch {
        fail("divide(1, 0) threw \(error), not CalcError")
    }

    greet("World", "Hello, World!")
    greet("", "Hello, !")
    greet("Wörld 🦀", "Hello, Wörld 🦀!")
    greet("a\0b", "Hello, a\0b!")
}

run()
expect(calculator_abi_version() == 4, "ABI revision 4")
expect(calculator_debug_live(-1) == 1, "the sample counts live resources")
assertNoLeaks()
print("swift/calculator: OK")
