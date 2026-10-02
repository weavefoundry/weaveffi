// Producer-side conformance: a C library that *implements* the generated
// calculator header instead of consuming a prebuilt one.
//
// A hand-written producer exports three groups of symbols, all prefixed with
// the header's prefix (`calculator`):
//
//   * the API's own functions (`calculator_calculator_add`, ...);
//   * the runtime surface every consumer relies on: the ABI revision, the
//     error helpers, `free_bytes` for returned strings and buffers, cancel
//     tokens, and the debug leak counter;
//   * one checksum function per top-level module, returning the constant the
//     header was generated with, so a consumer generated from a different
//     contract refuses to load.
//
// The harness builds this file with -fvisibility=hidden and checks with `nm`
// that every symbol is still exported (the header's CALCULATOR_API macro
// marks each prototype), then runs producer_check.c against it.
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

#include "calculator.h"

// Live returned allocations and cancel tokens, for calculator_debug_live.
static atomic_uint_fast64_t live_allocations;
static atomic_uint_fast64_t live_tokens;

// ── runtime ────────────────────────────────────────────────────────────────

uint32_t calculator_abi_version(void) { return CALCULATOR_ABI_VERSION; }

uint64_t calculator_calculator_checksum(void) { return CALCULATOR_CALCULATOR_CHECKSUM; }

void calculator_error_set(calculator_error* err, int32_t code, const char* message) {
    if (err == NULL) return;
    calculator_error_clear(err);
    err->code = code;
    if (message != NULL) {
        size_t n = strlen(message) + 1;
        char* copy = (char*)malloc(n);
        if (copy != NULL) memcpy(copy, message, n);
        err->message = copy;
    }
}

void calculator_error_clear(calculator_error* err) {
    if (err == NULL) return;
    free((void*)err->message);
    free((void*)err->payload_ptr);
    err->code = 0;
    err->message = NULL;
    err->payload_ptr = NULL;
    err->payload_len = 0;
}

void calculator_error_free(calculator_error* err) {
    calculator_error_clear(err);
    free(err);
}

void calculator_free_bytes(uint8_t* ptr, size_t len) {
    (void)len;
    if (ptr != NULL) {
        atomic_fetch_sub(&live_allocations, 1);
        free(ptr);
    }
}

struct calculator_cancel_token {
    atomic_int refs;
    atomic_bool cancelled;
};

calculator_cancel_token* calculator_cancel_token_create(void) {
    calculator_cancel_token* t = (calculator_cancel_token*)malloc(sizeof *t);
    if (t == NULL) return NULL;
    atomic_init(&t->refs, 1);
    atomic_init(&t->cancelled, false);
    atomic_fetch_add(&live_tokens, 1);
    return t;
}

void calculator_cancel_token_cancel(calculator_cancel_token* token) {
    if (token != NULL) atomic_store(&token->cancelled, true);
}

bool calculator_cancel_token_is_cancelled(const calculator_cancel_token* token) {
    return token != NULL && atomic_load(&((calculator_cancel_token*)token)->cancelled);
}

void calculator_cancel_token_destroy(calculator_cancel_token* token) {
    if (token != NULL && atomic_fetch_sub(&token->refs, 1) == 1) {
        atomic_fetch_sub(&live_tokens, 1);
        free(token);
    }
}

uint64_t calculator_debug_live(int32_t kind) {
    switch (kind) {
    case 3:
        return atomic_load(&live_tokens);
    case 4:
        return atomic_load(&live_allocations);
    default:
        return 0;
    }
}

// ── API ────────────────────────────────────────────────────────────────────

int32_t calculator_calculator_add(int32_t a, int32_t b, calculator_error* out_err) {
    (void)out_err;
    return a + b;
}

int32_t calculator_calculator_mul(int32_t a, int32_t b, calculator_error* out_err) {
    (void)out_err;
    return a * b;
}

int32_t calculator_calculator_div(int32_t a, int32_t b, calculator_error* out_err) {
    if (b == 0) {
        calculator_error_set(out_err, calculator_calculator_CalcError_DivisionByZero,
                             "division by zero");
        return 0;
    }
    return a / b;
}

// Strings arrive as borrowed (ptr, len) and return as an owned run the
// consumer releases with calculator_free_bytes; NULL + 0 is the empty string.
const uint8_t* calculator_calculator_echo(const uint8_t* s_ptr, size_t s_len, size_t* out_len,
                                          calculator_error* out_err) {
    *out_len = 0;
    if (s_ptr == NULL && s_len != 0) {
        calculator_error_set(out_err, -3, "null string with a nonzero length");
        return NULL;
    }
    if (s_len == 0) return NULL;
    uint8_t* copy = (uint8_t*)malloc(s_len);
    if (copy == NULL) {
        calculator_error_set(out_err, -1, "out of memory");
        return NULL;
    }
    memcpy(copy, s_ptr, s_len);
    atomic_fetch_add(&live_allocations, 1);
    *out_len = s_len;
    return copy;
}
