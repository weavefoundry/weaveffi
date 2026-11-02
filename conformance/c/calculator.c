// Conformance consumer: calculator sample, C target (ABI revision 6).
//
// The getting-started surface: a direct-value call, throwing calls that
// report the typed `CalcError` and `ParseError` domains (one with a payload)
// and an untyped (`throws: any`) failure, a string in and out (non-ASCII
// text and an interior NUL byte survive), an optional scalar return (an
// opt struct), and typed arrays in both directions (runs of packed
// elements). Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include "calculator_buffer.h"

HARNESS_RUNS(calculator)

static void greet(calculator_run name, const void* expected, size_t expected_len) {
    calculator_error err;
    calculator_run out = calculator_greet(name, &err);
    assert(err.code == 0);
    assert(out.len == expected_len && memcmp(out.ptr, expected, out.len) == 0);
    calculator_run_free(out);
}

static void parse_fails(const char* text, const char* message) {
    calculator_error err;
    assert(calculator_parse(str_run(text), &err) == 0);
    assert(err.code == calculator_ParseError_NotANumber);
    assert(MSG_EQ(err, message));
    calculator_ParseError_NotANumber_payload payload;
    assert(calculator_ParseError_NotANumber_payload_decode(err.payload_ptr, err.payload_len,
                                                           &payload));
    assert(bytes_eq((const uint8_t*)payload.text.ptr, payload.text.len, text));
    calculator_ParseError_NotANumber_payload_free(&payload);
    calculator_error_clear(&err);
}

static void running_total(const int32_t* values, size_t n, const int32_t* expected) {
    calculator_error err;
    calculator_run out = calculator_running_total(bytes_run(values, n * sizeof(int32_t)), &err);
    assert(err.code == 0 && out.len == n * sizeof(int32_t));
    assert(n == 0 || memcmp(out.ptr, expected, out.len) == 0);
    assert(((uintptr_t)out.ptr % 8) == 0 && "producer runs are 8-aligned");
    calculator_run_free(out);
}

int main(void) {
    // `out_err` is overwritten, never read: garbage in it is fine.
    calculator_error err;
    memset(&err, 0xA5, sizeof err);

    assert(calculator_abi_version() == CALCULATOR_ABI_VERSION);
    assert(calculator_contract_check() == NULL);

    assert(calculator_add(2, 3, &err) == 5 && err.code == 0);
    assert(calculator_add(-7, 7, &err) == 0);
    assert(calculator_add(INT32_MAX, 1, &err) == INT32_MIN && "wraps");

    assert(calculator_divide(10, 2, &err) == 5 && err.code == 0);
    assert(calculator_divide(-7, 2, &err) == -3 && "rounds toward zero");
    assert(calculator_divide(INT32_MIN, -1, &err) == INT32_MIN);

    assert(calculator_divide(1, 0, &err) == 0);
    assert(err.code == calculator_CalcError_DivisionByZero);
    assert(MSG_EQ(err, "division by zero"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message_ptr == NULL && err.message_len == 0);

    // A second domain whose code carries a payload. Its code (1) equals
    // CalcError's: a code means something only within the callable's domain.
    assert(calculator_parse(str_run("42"), &err) == 42 && err.code == 0);
    assert(calculator_parse(str_run(" 42 "), &err) == 42 && err.code == 0);
    parse_fails("4x", "not a number: 4x");
    parse_fails("", "not a number: ");

    // `throws: any`: code -1 and a message, no payload.
    assert(calculator_sqrt(9.0, &err) == 3.0 && err.code == 0);
    assert(calculator_sqrt(-4.0, &err) == 0.0);
    assert(err.code == -1 && MSG_EQ(err, "cannot take the square root of -4"));
    assert(err.payload_ptr == NULL);
    calculator_error_clear(&err);

    // An optional scalar return, a typed-array parameter.
    const double xs[3] = {1.0, 2.0, 6.0};
    calculator_opt_f64 mean = calculator_mean(bytes_run(xs, sizeof xs), &err);
    assert(err.code == 0 && mean.has && mean.value == 3.0);
    calculator_run empty = {NULL, 0};
    mean = calculator_mean(empty, &err);
    assert(err.code == 0 && !mean.has && mean.value == 0.0);

    // A typed-array parameter and return.
    const int32_t ints[4] = {1, 2, 3, INT32_MAX};
    const int32_t totals[4] = {1, 3, 6, -2147483643};
    running_total(ints, 3, totals);
    running_total(ints, 4, totals);
    running_total(NULL, 0, NULL);

    greet(str_run("World"), "Hello, World!", 13);
    greet(empty, "Hello, !", 8);
    greet(str_run("W\xc3\xb6rld \xf0\x9f\xa6\x80"), "Hello, W\xc3\xb6rld \xf0\x9f\xa6\x80!", 19);
    greet(bytes_run("a\0b", 3), "Hello, a\0b!", 11);

    // Invalid input is a marshalling failure (-3), not a crash.
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    calculator_run out = calculator_greet(bytes_run(bad_utf8, sizeof bad_utf8), &err);
    assert(out.ptr == NULL && out.len == 0);
    assert(err.code == -3 && err.message_len > 0);
    calculator_error_clear(&err);

    // A typed array whose length isn't a multiple of its element's, or
    // that isn't aligned for it, is -3 too.
    _Alignas(8) uint8_t raw[16] = {0};
    mean = calculator_mean(bytes_run(raw, 7), &err);
    assert(!mean.has && err.code == -3);
    calculator_error_clear(&err);
    mean = calculator_mean(bytes_run(raw + 1, 8), &err);
    assert(!mean.has && err.code == -3);
    calculator_error_clear(&err);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/calculator: OK\n");
    return 0;
}
