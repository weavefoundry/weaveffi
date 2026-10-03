// Drives the hand-written C producer (producer.c) through the generated
// calculator header exactly as a consumer of the Rust producer would: the
// load-time contract check, a call, a typed error, a (ptr, len) string round
// trip with an interior NUL, a cancel token, and the leak counters.

#include "harness.h"

#include "calculator.h"

int main(void) {
    calculator_error err = {0};

    assert(calculator_abi_version() == CALCULATOR_ABI_VERSION);
    assert(calculator_calculator_checksum() == CALCULATOR_CALCULATOR_CHECKSUM);

    assert(calculator_calculator_add(2, 3, &err) == 5 && err.code == 0);
    assert(calculator_calculator_mul(4, -5, &err) == -20);
    assert(calculator_calculator_div(9, 0, &err) == 0);
    assert(err.code == calculator_calculator_CalcError_DivisionByZero);
    assert(strcmp(err.message, "division by zero") == 0);
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    static const char text[] = "a\0b";
    size_t len = 0;
    const uint8_t* echoed =
        calculator_calculator_echo((const uint8_t*)text, sizeof text - 1, &len, &err);
    assert(err.code == 0 && len == 3 && memcmp(echoed, text, 3) == 0);
    assert(calculator_debug_live(4) == 1);
    calculator_free_bytes((uint8_t*)echoed, len);

    calculator_cancel_token* token = calculator_cancel_token_create();
    assert(!calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_cancel(token);
    assert(calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_destroy(token);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/producer: OK\n");
    return 0;
}
