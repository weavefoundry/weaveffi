// Conformance consumer: calculator sample, C target (ABI revision 4).
//
// The getting-started surface: a direct-value call, a throwing call that
// reports the typed `CalcError` domain, and a string in and out (non-ASCII
// text and an interior NUL byte survive). Ends by asserting the producer's
// leak counters are zero.

#include "harness.h"

#include "calculator.h"

static void greet(const void* name, size_t name_len, const void* expected, size_t expected_len) {
    calculator_error err = {0};
    size_t len = 0;
    const uint8_t* out =
        calculator_calculator_greet((const uint8_t*)name, name_len, &len, &err);
    assert(err.code == 0);
    assert(len == expected_len && memcmp(out, expected, len) == 0);
    calculator_free_bytes((uint8_t*)out, len);
}

int main(void) {
    calculator_error err = {0};

    assert(calculator_abi_version() == CALCULATOR_ABI_VERSION);
    assert(calculator_calculator_contract_check() == 0);

    assert(calculator_calculator_add(2, 3, &err) == 5 && err.code == 0);
    assert(calculator_calculator_add(-7, 7, &err) == 0);
    assert(calculator_calculator_add(INT32_MAX, 1, &err) == INT32_MIN && "wraps");

    assert(calculator_calculator_divide(10, 2, &err) == 5 && err.code == 0);
    assert(calculator_calculator_divide(-7, 2, &err) == -3 && "rounds toward zero");
    assert(calculator_calculator_divide(INT32_MIN, -1, &err) == INT32_MIN);

    assert(calculator_calculator_divide(1, 0, &err) == 0);
    assert(err.code == calculator_calculator_CalcError_DivisionByZero);
    assert(strcmp(err.message, "division by zero") == 0);
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    greet(STR("World"), STR("Hello, World!"));
    greet(NULL, 0, STR("Hello, !"));
    greet(STR("W\xc3\xb6rld \xf0\x9f\xa6\x80"), STR("Hello, W\xc3\xb6rld \xf0\x9f\xa6\x80!"));
    greet("a\0b", 3, "Hello, a\0b!", 11);

    // Invalid input is a marshalling failure (-3), not a crash.
    size_t len = 0;
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    assert(calculator_calculator_greet(BYTES(bad_utf8), &len, &err) == NULL);
    assert(err.code == -3);
    calculator_error_clear(&err);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/calculator: OK\n");
    return 0;
}
