// Drives the hand-written C producer (producer.c) through the generated
// calculator header exactly as a consumer of the Rust producer would: the
// load-time contract check, calls, typed errors (one with a payload) and an
// untyped one, a (ptr, len) string in and out with an interior NUL, an
// optional scalar return, typed arrays in both directions, a
// consumer-allocated run, a cancel token, and the leak counters.

#include "harness.h"

#include "calculator_buffer.h"

int main(void) {
    calculator_error err = {0};

    assert(calculator_abi_version() == CALCULATOR_ABI_VERSION);
    assert(calculator_calculator_contract_check() == 0);
    assert(calculator_debug_live(-1) == 1);

    assert(calculator_calculator_add(2, 3, &err) == 5 && err.code == 0);
    assert(calculator_calculator_add(INT32_MAX, 1, &err) == INT32_MIN);
    assert(calculator_calculator_divide(-7, 2, &err) == -3 && err.code == 0);
    assert(calculator_calculator_divide(9, 0, &err) == 0);
    assert(err.code == calculator_calculator_CalcError_DivisionByZero);
    assert(MSG_EQ(err, "division by zero"));
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message_ptr == NULL && err.message_len == 0);

    assert(calculator_calculator_parse(STR(" -12 "), &err) == -12 && err.code == 0);
    assert(calculator_calculator_parse(STR("4x"), &err) == 0);
    assert(err.code == calculator_calculator_ParseError_NotANumber);
    assert(MSG_EQ(err, "not a number: 4x"));
    calculator_calculator_ParseError_NotANumber_payload payload;
    assert(calculator_calculator_ParseError_NotANumber_payload_decode(err.payload_ptr,
                                                                      err.payload_len, &payload));
    assert(bytes_eq((const uint8_t*)payload.text.ptr, payload.text.len, "4x"));
    calculator_calculator_ParseError_NotANumber_payload_free(&payload);
    calculator_error_clear(&err);

    assert(calculator_calculator_sqrt(-4.0, &err) == 0.0);
    assert(err.code == -1 && MSG_EQ(err, "cannot take the square root of -4"));
    calculator_error_clear(&err);

    const double xs[2] = {1.0, 2.0};
    double mean = 0.0;
    assert(calculator_calculator_mean(xs, 2, &mean, &err) && mean == 1.5);
    assert(!calculator_calculator_mean(NULL, 0, &mean, &err) && err.code == 0);

    const int32_t ints[3] = {1, 2, 3};
    size_t count = 0;
    int32_t* totals = calculator_calculator_running_total(ints, 3, &count, &err);
    assert(err.code == 0 && count == 3 && totals[2] == 6);
    calculator_free_bytes((uint8_t*)totals, count * sizeof(int32_t));

    static const char name[] = "a\0b";
    size_t len = 0;
    const uint8_t* greeting =
        calculator_calculator_greet((const uint8_t*)name, sizeof name - 1, &len, &err);
    assert(err.code == 0 && len == 11 && memcmp(greeting, "Hello, a\0b!", 11) == 0);
    assert(calculator_debug_live(4) == 1);
    calculator_free_bytes((uint8_t*)greeting, len);

    uint8_t* staged = calculator_alloc(8);
    assert(staged != NULL && calculator_alloc(0) == NULL);
    assert(((uintptr_t)staged % 8) == 0);
    calculator_error_set(&err, calculator_calculator_CalcError_DivisionByZero, STR("with payload"));
    calculator_error_set_payload(&err, staged, 8);
    assert(err.payload_len == 8 && MSG_EQ(err, "with payload"));
    calculator_error_clear(&err);
    calculator_free_bytes(staged, 8);

    calculator_cancel_token* token = calculator_cancel_token_create();
    assert(!calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_cancel(token);
    assert(calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_destroy(token);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/producer: OK\n");
    return 0;
}
