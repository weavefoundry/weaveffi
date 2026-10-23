// Conformance consumer: calculator sample, C target (ABI revision 5).
//
// The getting-started surface: a direct-value call, throwing calls that
// report the typed `CalcError` and `ParseError` domains (one with a payload)
// and an untyped (`throws: any`) failure, a string in and out (non-ASCII
// text and an interior NUL byte survive), an optional scalar return
// (OptDirect), and typed arrays in both directions (Slice). Ends by asserting
// the producer's leak counters are zero.

#include "harness.h"

#include "calculator_buffer.h"

static void greet(const void* name, size_t name_len, const void* expected, size_t expected_len) {
    calculator_error err = {0};
    size_t len = 0;
    const uint8_t* out =
        calculator_calculator_greet((const uint8_t*)name, name_len, &len, &err);
    assert(err.code == 0);
    assert(len == expected_len && memcmp(out, expected, len) == 0);
    calculator_free_bytes((uint8_t*)out, len);
}

static void parse_fails(const char* text, const char* message) {
    calculator_error err = {0};
    assert(calculator_calculator_parse(STR(text), &err) == 0);
    assert(err.code == calculator_calculator_ParseError_NotANumber);
    assert(MSG_EQ(err, message));
    calculator_calculator_ParseError_NotANumber_payload payload;
    assert(calculator_calculator_ParseError_NotANumber_payload_decode(err.payload_ptr,
                                                                      err.payload_len, &payload));
    assert(bytes_eq((const uint8_t*)payload.text.ptr, payload.text.len, text));
    calculator_calculator_ParseError_NotANumber_payload_free(&payload);
    calculator_error_clear(&err);
}

static void running_total(const int32_t* values, size_t n, const int32_t* expected) {
    calculator_error err = {0};
    size_t len = 99;
    int32_t* out = calculator_calculator_running_total(values, n, &len, &err);
    assert(err.code == 0 && len == n);
    assert(n == 0 || memcmp(out, expected, n * sizeof(int32_t)) == 0);
    assert(((uintptr_t)out % 8) == 0 && "producer runs are 8-aligned");
    calculator_free_bytes((uint8_t*)out, len * sizeof(int32_t));
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
    assert(MSG_EQ(err, "division by zero"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message_ptr == NULL && err.message_len == 0);

    // A second domain whose code carries a payload. Its code (1) equals
    // CalcError's: a code means something only within the callable's domain.
    assert(calculator_calculator_parse(STR("42"), &err) == 42 && err.code == 0);
    assert(calculator_calculator_parse(STR(" 42 "), &err) == 42 && err.code == 0);
    parse_fails("4x", "not a number: 4x");
    parse_fails("", "not a number: ");

    // `throws: any`: code -1 and a message, no payload.
    assert(calculator_calculator_sqrt(9.0, &err) == 3.0 && err.code == 0);
    assert(calculator_calculator_sqrt(-4.0, &err) == 0.0);
    assert(err.code == -1 && MSG_EQ(err, "cannot take the square root of -4"));
    assert(err.payload_ptr == NULL);
    calculator_error_clear(&err);

    // OptDirect return, Slice parameter.
    const double xs[3] = {1.0, 2.0, 6.0};
    double mean = -1.0;
    assert(calculator_calculator_mean(xs, 3, &mean, &err) && err.code == 0 && mean == 3.0);
    assert(!calculator_calculator_mean(NULL, 0, &mean, &err) && err.code == 0);

    // Slice parameter and return.
    const int32_t ints[4] = {1, 2, 3, INT32_MAX};
    const int32_t totals[4] = {1, 3, 6, -2147483643};
    running_total(ints, 3, totals);
    running_total(ints, 4, totals);
    running_total(NULL, 0, NULL);

    greet(STR("World"), STR("Hello, World!"));
    greet(NULL, 0, STR("Hello, !"));
    greet(STR("W\xc3\xb6rld \xf0\x9f\xa6\x80"), STR("Hello, W\xc3\xb6rld \xf0\x9f\xa6\x80!"));
    greet("a\0b", 3, "Hello, a\0b!", 11);

    // Invalid input is a marshalling failure (-3), not a crash.
    size_t len = 0;
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    assert(calculator_calculator_greet(BYTES(bad_utf8), &len, &err) == NULL);
    assert(err.code == -3 && err.message_len > 0);
    calculator_error_clear(&err);

    // A typed array that isn't aligned for its element type is -3 too.
    _Alignas(8) uint8_t raw[16] = {0};
    assert(!calculator_calculator_mean((const double*)(const void*)(raw + 1), 1, &mean, &err));
    assert(err.code == -3);
    calculator_error_clear(&err);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/calculator: OK\n");
    return 0;
}
