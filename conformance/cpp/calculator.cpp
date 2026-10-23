// Conformance consumer: calculator sample, C++ target (ABI revision 5).
//
// The getting-started surface: the load-time check, a direct-value call
// (wrapping), throwing calls whose typed exceptions come from two domains of
// one module (`CalcError` and `ParseError`, whose code 1 values differ and
// whose `NotANumberError` carries a field), an untyped (`throws: any`)
// failure thrown as the root `Error` with code -1, a string in and out
// through `std::string_view` (non-ASCII text, an astral character, and an
// interior NUL survive the pointer-and-length ABI), an optional scalar
// return (`std::optional<double>`), and typed arrays in both directions
// (`std::vector` storage passed as is). Invalid UTF-8 is a producer-side
// marshalling failure on a call that declares no errors, so it throws
// InternalError, which is an Error too. Ends by asserting the producer's
// leak counters are all zero.

#include <climits>
#include <cstdio>
#include <optional>
#include <string>
#include <vector>

#include "calculator.hpp"
#include "check.hpp"

namespace calc = ::calculator::calculator;
using calculator::CalcError;
using calculator::DivisionByZeroError;
using calculator::Error;
using calculator::InternalError;
using calculator::NotANumberError;
using calculator::ParseError;

static void arithmetic() {
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
}

static void second_domain() {
    CHECK(calc::parse("42") == 42);
    CHECK(calc::parse(" 42 ") == 42);

    // ParseError's code 1 is NotANumber, not CalcError's DivisionByZero.
    NotANumberError e = expect_throw<NotANumberError>([] { calc::parse("4x"); }, "parse('4x')");
    CHECK(e.code() == 1 && e.text == "4x");
    CHECK(std::string(e.what()) == "not a number: 4x");
    CHECK(dynamic_cast<const ParseError*>(&e) != nullptr);
    CHECK(dynamic_cast<const CalcError*>(&e) == nullptr);

    e = expect_throw<NotANumberError>([] { calc::parse(""); }, "parse('')");
    CHECK(e.text.empty() && std::string(e.what()) == "not a number: ");
}

static void untyped() {
    CHECK(calc::sqrt(9.0) == 3.0);
    Error e = expect_throw<Error>([] { calc::sqrt(-4.0); }, "sqrt(-4)");
    CHECK(e.code() == -1);
    CHECK(std::string(e.what()) == "cannot take the square root of -4");
    CHECK(dynamic_cast<const InternalError*>(&e) == nullptr);
}

static void strings() {
    CHECK(calc::greet("World") == "Hello, World!");
    CHECK(calc::greet("") == "Hello, !");
    CHECK(calc::greet("W\xc3\xb6rld \xf0\x9f\xa6\x80") == "Hello, W\xc3\xb6rld \xf0\x9f\xa6\x80!");
    const std::string with_nul("a\0b", 3);
    const std::string greeted = calc::greet(with_nul);
    CHECK(greeted == std::string("Hello, a\0b!", 11));

    InternalError bad = expect_throw<InternalError>([] { calc::greet("\xc3\x28"); }, "greet(non-UTF-8)");
    CHECK(bad.code() == -3);
    CHECK(dynamic_cast<const Error*>(&bad) != nullptr);
}

static void optional_and_slices() {
    std::optional<double> m = calc::mean({1.0, 2.0, 6.0});
    CHECK(m.has_value() && *m == 3.0);
    CHECK(!calc::mean({}).has_value());

    CHECK((calc::running_total({1, 2, 3}) == std::vector<int32_t>{1, 3, 6}));
    CHECK(calc::running_total({}).empty());
    CHECK((calc::running_total({1, 2, 3, INT32_MAX}) == std::vector<int32_t>{1, 3, 6, -2147483643}));

    // A vector passes its own storage, so a large one crosses without a copy.
    std::vector<int32_t> ones(100000, 1);
    std::vector<int32_t> totals = calc::running_total(ones);
    CHECK(totals.size() == ones.size() && totals.back() == 100000);
}

int main() {
    calculator::check_library();
    arithmetic();
    second_domain();
    untyped();
    strings();
    optional_and_slices();

    check_no_leaks(calculator_debug_live, "calculator");
    std::printf("cpp/calculator: OK\n");
    return 0;
}
