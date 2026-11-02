// Drives the hand-written C producer (producer.c) through the generated
// calculator header exactly as a consumer of the Rust producer would: the
// load-time checks (the ABI revision and the contract functions its
// CALCULATOR_DEFINE_CONTRACT defined), calls with an `out_err` full of
// garbage (overwritten, never read), typed errors (one with a payload) and
// an untyped one, a string run in and out with an interior NUL, an optional
// scalar return (an opt struct), typed arrays in both directions (runs with
// `len` in bytes), marshalling failures, a consumer-allocated run, a cancel
// token, and the leak counters.

#include "harness.h"

#include "calculator_buffer.h"

HARNESS_RUNS(calculator)

int main(void) {
    calculator_error err;
    memset(&err, 0xA5, sizeof err);

    assert(calculator_abi_version() == CALCULATOR_ABI_VERSION && CALCULATOR_ABI_VERSION == 6u);
    assert(calculator_contract_check() == NULL);
    assert(fnv1a64("add") == 0xe70bc3190530e654ull);
    assert(calculator_contract_e70bc3190530e654() == fnv1a64("function add(i32, i32) -> i32"));
    assert(calculator_contract_59166416d94c7a25() == fnv1a64("code NotANumber = 1 {string}"));
    assert(calculator_debug_live(-1) == 1);

    // Success overwrites the garbage with a cleared error.
    assert(calculator_add(2, 3, &err) == 5);
    assert(err.code == 0 && err.message_ptr == NULL && err.message_len == 0);
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    assert(calculator_add(INT32_MAX, 1, &err) == INT32_MIN);
    assert(calculator_divide(-7, 2, &err) == -3 && err.code == 0);
    assert(calculator_divide(9, 0, &err) == 0);
    assert(err.code == calculator_CalcError_DivisionByZero);
    assert(MSG_EQ(err, "division by zero"));
    calculator_error_clear(&err);
    assert(err.code == 0 && err.message_ptr == NULL && err.message_len == 0);

    assert(calculator_parse(str_run(" -12 "), &err) == -12 && err.code == 0);
    assert(calculator_parse(str_run("4x"), &err) == 0);
    assert(err.code == calculator_ParseError_NotANumber);
    assert(MSG_EQ(err, "not a number: 4x"));
    calculator_ParseError_NotANumber_payload payload;
    assert(calculator_ParseError_NotANumber_payload_decode(err.payload_ptr, err.payload_len,
                                                           &payload));
    assert(bytes_eq((const uint8_t*)payload.text.ptr, payload.text.len, "4x"));
    calculator_ParseError_NotANumber_payload_free(&payload);
    calculator_error_clear(&err);

    assert(calculator_sqrt(-4.0, &err) == 0.0);
    assert(err.code == -1 && MSG_EQ(err, "cannot take the square root of -4"));
    assert(err.payload_ptr == NULL);
    calculator_error_clear(&err);

    const double xs[2] = {1.0, 2.0};
    calculator_opt_f64 mean = calculator_mean(bytes_run(xs, sizeof xs), &err);
    assert(err.code == 0 && mean.has && mean.value == 1.5);
    calculator_run empty = {NULL, 0};
    mean = calculator_mean(empty, &err);
    assert(err.code == 0 && !mean.has && mean.value == 0.0);

    const int32_t ints[3] = {1, 2, 3};
    calculator_run totals = calculator_running_total(bytes_run(ints, sizeof ints), &err);
    assert(err.code == 0 && totals.len == sizeof ints && ((uintptr_t)totals.ptr % 8) == 0);
    const int32_t* t = (const int32_t*)(const void*)totals.ptr;
    assert(t[0] == 1 && t[1] == 3 && t[2] == 6);
    calculator_run_free(totals);

    static const char name[] = "a\0b";
    calculator_run greeting = calculator_greet(bytes_run(name, sizeof name - 1), &err);
    assert(err.code == 0 && greeting.len == 11 && memcmp(greeting.ptr, "Hello, a\0b!", 11) == 0);
    assert(calculator_debug_live(4) == 1);
    calculator_run_free(greeting);
    assert(calculator_debug_live(4) == 0);
    calculator_run_free(empty);  // the empty run is a no-op

    // Marshalling failures: invalid UTF-8, a null run with a length, and a
    // typed array that's ragged or misaligned.
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    greeting = calculator_greet(bytes_run(bad_utf8, sizeof bad_utf8), &err);
    assert(greeting.ptr == NULL && greeting.len == 0 && err.code == -3 && err.message_len > 0);
    calculator_error_clear(&err);
    calculator_run null_run = {NULL, 4};
    assert(calculator_parse(null_run, &err) == 0 && err.code == -3);
    calculator_error_clear(&err);
    _Alignas(8) uint8_t raw[16] = {0};
    mean = calculator_mean(bytes_run(raw, 7), &err);
    assert(!mean.has && err.code == -3);
    calculator_error_clear(&err);
    totals = calculator_running_total(bytes_run(raw + 1, 8), &err);
    assert(totals.ptr == NULL && err.code == -3);
    calculator_error_clear(&err);

    // A consumer-allocated run (as a callback would return) and an error
    // payload the error helpers copy.
    uint8_t* staged = calculator_alloc(8);
    assert(staged != NULL && calculator_alloc(0) == NULL);
    assert(((uintptr_t)staged % 8) == 0);
    memset(&err, 0, sizeof err);
    calculator_error_set(&err, calculator_CalcError_DivisionByZero,
                         (const uint8_t*)"with payload", 12);
    calculator_error_set_payload(&err, staged, 8);
    assert(err.payload_len == 8 && MSG_EQ(err, "with payload"));
    calculator_error_clear(&err);
    calculator_run staged_run = {staged, 8};
    calculator_run_free(staged_run);

    calculator_cancel_token* token = calculator_cancel_token_create();
    assert(!calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_cancel(token);
    assert(calculator_cancel_token_is_cancelled(token));
    calculator_cancel_token_destroy(token);
    assert(!calculator_cancel_token_is_cancelled(NULL));
    calculator_cancel_token_destroy(NULL);

    ASSERT_NO_LEAKS(calculator_debug_live);
    printf("c/producer: OK\n");
    return 0;
}
