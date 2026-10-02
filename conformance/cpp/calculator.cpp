// Conformance consumer: calculator sample, C++ target.
//
// Covers the smallest surface: direct scalars, a throwing function whose
// typed DivisionByZeroError derives from CalcError and Error, a string round
// trip through `std::string_view` (including an interior NUL, which the
// pointer-and-length ABI preserves), and the explicit library check.
// Ends by asserting the producer's leak counters are all zero.

#include <cstdio>
#include <string>

#include "calculator.hpp"
#include "check.hpp"

using namespace calculator;
namespace calc = ::calculator::calculator;

static void run() {
    check_library();

    CHECK(calc::add(2, 3) == 5);
    CHECK(calc::mul(-4, 6) == -24);
    CHECK(calc::div(17, 5) == 3);

    bool caught = false;
    try {
        calc::div(1, 0);
    } catch (const DivisionByZeroError& e) {
        caught = e.code() == 1 && std::string(e.what()) == "division by zero";
        CHECK(dynamic_cast<const CalcError*>(&e) != nullptr);
        CHECK(dynamic_cast<const Error*>(&e) != nullptr);
    }
    CHECK(caught);

    CHECK(calc::echo("hello") == "hello");
    CHECK(calc::echo("").empty());
    CHECK(calc::echo("héllo ✓") == "héllo ✓");
    const std::string with_nul("a\0b\0", 4);
    const std::string echoed = calc::echo(with_nul);
    CHECK(echoed.size() == 4);
    CHECK(echoed == with_nul);
}

int main() {
    run();
    check_no_leaks(calculator_debug_live, "calculator");
    std::printf("cpp/calculator: OK\n");
    return 0;
}
