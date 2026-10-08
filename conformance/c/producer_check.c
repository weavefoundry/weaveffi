// Drives the hand-written C producer (producer.c) through the generated
// calculator header exactly as a consumer of the Rust producer would: the
// load-time contract check, calls, a typed error (and an error payload), a
// (ptr, len) string in and out with an interior NUL, a consumer-allocated
// run, a cancel token, and the leak counters.

#include "harness.h"

#include "calculator.h"

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
    assert(strcmp(err.message, "division by zero") == 0);
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    static const char name[] = "a\0b";
    size_t len = 0;
    const uint8_t* greeting =
        calculator_calculator_greet((const uint8_t*)name, sizeof name - 1, &len, &err);
    assert(err.code == 0 && len == 11 && memcmp(greeting, "Hello, a\0b!", 11) == 0);
    assert(calculator_debug_live(4) == 1);
    calculator_free_bytes((uint8_t*)greeting, len);

    uint8_t* staged = calculator_alloc(8);
    assert(staged != NULL && calculator_alloc(0) == NULL);
    calculator_error_set(&err, calculator_calculator_CalcError_DivisionByZero, "with payload");
    calculator_error_set_payload(&err, staged, 8);
    assert(err.payload_len == 8);
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
