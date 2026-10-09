// Conformance consumer: calculator sample, C++ target (ABI revision 4).
//
// The getting-started surface: the load-time check, a direct-value call
// (wrapping), a throwing call whose typed DivisionByZeroError derives from
// CalcError and Error, and a string in and out through `std::string_view`
// (non-ASCII text, an astral character, and an interior NUL survive the
// pointer-and-length ABI). Invalid UTF-8 is a producer-side marshalling
// failure on a call that declares no errors, so it throws InternalError.
// Ends by asserting the producer's leak counters are all zero.

#include <climits>
#include <cstdio>
#include <string>

#include "calculator.hpp"
#include "check.hpp"

namespace calc = ::calculator::calculator;
using calculator::CalcError;
using calculator::DivisionByZeroError;
using calculator::Error;
using calculator::InternalError;

static void run() {
    calculator::check_library();

    CHECK(calc::add(2, 3) == 5);
    CHECK(calc::add(-7, 7) == 0);
    CHECK(calc::add(INT32_MAX, 1) == INT32_MIN);

    CHECK(calc::divide(10, 2) == 5);
    CHECK(calc::divide(-7, 2) == -3);
    CHECK(calc::divide(INT32_MIN, -1) == INT32_MIN);

    DivisionByZeroError e = expect_throw<DivisionByZeroError>([] { calc::divide(1, 0); }, "divide(1, 0)");
    CHECK(e.code() == 1);
    CHECK(std::string(e.what()) == "division by zero");
    CHECK(dynamic_cast<const CalcError*>(&e) != nullptr);
    CHECK(dynamic_cast<const Error*>(&e) != nullptr);

    CHECK(calc::greet("World") == "Hello, World!");
    CHECK(calc::greet("") == "Hello, !");
    CHECK(calc::greet("W\xc3\xb6rld \xf0\x9f\xa6\x80") == "Hello, W\xc3\xb6rld \xf0\x9f\xa6\x80!");
    const std::string with_nul("a\0b", 3);
    const std::string greeted = calc::greet(with_nul);
    CHECK(greeted == std::string("Hello, a\0b!", 11));

    InternalError bad = expect_throw<InternalError>([] { calc::greet("\xc3\x28"); }, "greet(non-UTF-8)");
    CHECK(bad.code() == -3);
}

int main() {
    run();
    check_no_leaks(calculator_debug_live, "calculator");
    std::printf("cpp/calculator: OK\n");
    return 0;
}
