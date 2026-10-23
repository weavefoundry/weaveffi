// Conformance consumer: calculator sample, Swift target (ABI revision 5).
//
// The getting-started surface through the generated `Calculator` module:
//
//   * the load check: `CalculatorLibrary.check()` passes, and the contract
//     comparison it runs reports a missing or changed declaration as a
//     `LoadError` (driven directly through the runtime, since a matching
//     library can't produce one);
//   * a direct-value call (wrapping on overflow) and a string in and out
//     (non-ASCII text, an astral character, and an interior NUL survive);
//   * two error domains whose codes coincide: `CalcError.divisionByZero` and
//     `ParseError.notANumber` with its `text` field, dispatched by the
//     callable's domain; an unknown positive code of a domain (from a newer
//     library) decodes to the domain's `unknown` case;
//   * `throws: any`: `sqrt` fails with the runtime error, code -1, and the
//     producer's message;
//   * an optional scalar return (`mean -> Double?`) and typed arrays in and
//     out (`runningTotal`), including empty arrays and wrapping.
//
// Ends by asserting the producer's leak counters are zero. Exits non-zero on
// any mismatch.

import CCalculator
@testable import Calculator
import Foundation

func fail(_ msg: String, line: UInt = #line) -> Never {
    FileHandle.standardError.write(Data("assertion failed (line \(line)): \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: @autoclosure () -> String, line: UInt = #line) {
    if !cond { fail(msg(), line: line) }
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

func loadCheck() {
    do {
        try CalculatorLibrary.check()
        try CalculatorLibrary.check()
    } catch {
        fail("check() threw \(error)")
    }
    expect(CalculatorLibrary.abiVersion == 5, "abiVersion")

    // The comparison `check()` runs, against the real table: an unknown id
    // is missing, a known id with another hash changed.
    let missing = wvCheckContract(calculator_calculator_contract, [(0x1, 0x2, "calculator.nope")])
    expect(missing == .missing(declaration: "calculator.nope"), "missing row (got \(String(describing: missing)))")
    var len = 0
    guard let first = calculator_calculator_contract(&len)?.pointee, len > 0 else { fail("empty contract table") }
    let changed = wvCheckContract(calculator_calculator_contract, [(first.id, first.hash &+ 1, "calculator.x")])
    expect(changed == .changed(declaration: "calculator.x"), "changed row (got \(String(describing: changed)))")
    expect(wvCheckContract(calculator_calculator_contract, [(first.id, first.hash, "calculator.x")]) == nil, "matching row")
    let description = CalculatorLibrary.LoadError.missing(declaration: "calculator.nope").localizedDescription
    expect(description == "Calculator: calculator.nope is missing from the library 'calculator'; regenerate the bindings or rebuild the library", description)
}

func domains() {
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
        expect(error == .divisionByZero(message: "division by zero"), "divide(1, 0) threw \(error)")
        expect(error.errorCode == 1 && error.message == "division by zero", "DivisionByZero code and message")
        expect(error.localizedDescription == "division by zero", "localizedDescription")
    } catch {
        fail("divide(1, 0) threw \(error), not CalcError")
    }

    // A second domain with a payload; its code 1 is CalcError's too.
    do {
        expect(try Calculator.parse(text: "42") == 42, "parse(42)")
        expect(try Calculator.parse(text: " 42 ") == 42, "parse( 42 )")
    } catch {
        fail("parse threw \(error)")
    }
    for (text, message) in [("4x", "not a number: 4x"), ("", "not a number: ")] {
        do {
            _ = try Calculator.parse(text: text)
            fail("parse(\(text.debugDescription)) returned")
        } catch let error as ParseError {
            expect(error == .notANumber(message: message, text: text), "parse(\(text.debugDescription)) threw \(error)")
            expect(error.errorCode == 1, "NotANumber code")
        } catch {
            fail("parse(\(text.debugDescription)) threw \(error), not ParseError")
        }
    }

    // Domains are open: a code these bindings don't know keeps its code and
    // message in the domain's `unknown` case.
    var raw = calculator_error()
    let message = Array("from a newer library".utf8)
    calculator_error_set(&raw, 77, message, message.count)
    let unknown = CalcError(wvError: raw)
    expect(unknown == .unknown(code: 77, message: "from a newer library"), "unknown code (got \(unknown))")
    expect(unknown.errorCode == 77, "unknown errorCode")
    calculator_error_clear(&raw)
}

func untyped() {
    do {
        expect(try Calculator.sqrt(x: 9) == 3, "sqrt(9)")
        _ = try Calculator.sqrt(x: -4)
        fail("sqrt(-4) returned")
    } catch let error as CalculatorRuntimeError {
        expect(error.errorCode == CalculatorRuntimeError.untypedCode, "sqrt(-4) code \(error.errorCode)")
        expect(error.message == "cannot take the square root of -4", "sqrt(-4) message \(error.message)")
    } catch {
        fail("sqrt threw \(error)")
    }
}

func scalarsAndArrays() {
    expect(Calculator.add(a: 2, b: 3) == 5, "add(2, 3)")
    expect(Calculator.add(a: -7, b: 7) == 0, "add(-7, 7)")
    expect(Calculator.add(a: .max, b: 1) == .min, "add wraps")

    expect(Calculator.mean(values: [1, 2, 6]) == 3, "mean([1, 2, 6])")
    expect(Calculator.mean(values: []) == nil, "mean([]) is nil")

    expect(Calculator.runningTotal(values: [1, 2, 3]) == [1, 3, 6], "runningTotal([1, 2, 3])")
    expect(Calculator.runningTotal(values: []) == [], "runningTotal([])")
    expect(Calculator.runningTotal(values: [1, 2, 3, .max]) == [1, 3, 6, -2_147_483_643], "runningTotal wraps")
    // A slice of an array lends its own (offset) storage.
    let longer: [Int32] = [9, 1, 2, 3]
    expect(Calculator.runningTotal(values: Array(longer[1...])) == [1, 3, 6], "runningTotal of a slice")
}

func run() {
    loadCheck()
    domains()
    untyped()
    scalarsAndArrays()
    greet("World", "Hello, World!")
    greet("", "Hello, !")
    greet("Wörld 🦀", "Hello, Wörld 🦀!")
    greet("a\0b", "Hello, a\0b!")
}

run()
expect(calculator_abi_version() == 5, "ABI revision 5")
expect(calculator_debug_live(-1) == 1, "the sample counts live resources")
assertNoLeaks()
print("swift/calculator: OK")
