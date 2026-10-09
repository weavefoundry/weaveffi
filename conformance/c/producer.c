// Producer-side conformance: a C library that *implements* the generated
// calculator header instead of consuming a prebuilt one.
//
// A hand-written producer exports three groups of symbols, all prefixed with
// the header's prefix (`calculator`):
//
//   * the API's own functions (`calculator_calculator_add`, ...);
//   * the runtime surface every consumer relies on: the ABI revision, the
//     error helpers, `alloc` and `free_bytes` for byte runs, cancel tokens,
//     and the debug leak counter;
//   * one contract table per top-level module, returning the entries the
//     header was generated with, so a consumer generated from a different
//     contract refuses to load and names what changed.
//
// The harness builds this file with -fvisibility=hidden and checks with `nm`
// that every symbol is still exported (the header's CALCULATOR_API macro
// marks each prototype), then runs producer_check.c against it.
#include <math.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "calculator_buffer.h"

// Live runs (returned, consumer-allocated, and error messages and payloads)
// and cancel tokens, for calculator_debug_live.
static atomic_uint_fast64_t live_allocations;
static atomic_uint_fast64_t live_tokens;

// ── runs ───────────────────────────────────────────────────────────────────

// Every run this library hands out, and every run a consumer allocates with
// calculator_alloc, is one 8-aligned block (malloc's alignment is at least
// 8 on every supported platform), so calculator_free_bytes releases both.
static uint8_t* run_new(size_t len) {
    if (len == 0) return NULL;
    uint8_t* run = (uint8_t*)calloc(len, 1);
    if (run != NULL) atomic_fetch_add(&live_allocations, 1);
    return run;
}

static uint8_t* run_copy(const void* data, size_t len) {
    uint8_t* run = run_new(len);
    if (run != NULL) memcpy(run, data, len);
    return run;
}

static void run_free(const uint8_t* run) {
    if (run != NULL) {
        atomic_fetch_sub(&live_allocations, 1);
        free((void*)run);
    }
}

// ── runtime ────────────────────────────────────────────────────────────────

uint32_t calculator_abi_version(void) { return CALCULATOR_ABI_VERSION; }

// The table the header was generated with is exactly what this producer
// implements, so it returns the header's own entries (sorted by id).
const calculator_contract_entry* calculator_calculator_contract(size_t* out_len) {
    static const calculator_contract_entry table[] = CALCULATOR_CALCULATOR_CONTRACT;
    if (out_len != NULL) *out_len = CALCULATOR_CALCULATOR_CONTRACT_LEN;
    return table;
}

// The message is length-delimited UTF-8 (no NUL terminator); NULL is the
// empty message. A producer must not fail on a malformed one, and this one
// copies the bytes as they are.
void calculator_error_set(calculator_error* err, int32_t code, const uint8_t* message_ptr,
                          size_t message_len) {
    if (err == NULL) return;
    calculator_error_clear(err);
    err->code = code;
    if (message_ptr != NULL && message_len != 0) {
        err->message_ptr = run_copy(message_ptr, message_len);
        err->message_len = err->message_ptr != NULL ? message_len : 0;
    }
}

void calculator_error_set_payload(calculator_error* err, const uint8_t* ptr, size_t len) {
    if (err == NULL) return;
    run_free(err->payload_ptr);
    err->payload_ptr = NULL;
    err->payload_len = 0;
    if (ptr == NULL || len == 0) return;
    err->payload_ptr = run_copy(ptr, len);
    err->payload_len = err->payload_ptr != NULL ? len : 0;
}

void calculator_error_clear(calculator_error* err) {
    if (err == NULL) return;
    run_free(err->message_ptr);
    run_free(err->payload_ptr);
    memset(err, 0, sizeof *err);
}

void calculator_error_free(calculator_error* err) {
    calculator_error_clear(err);
    free(err);
}

uint8_t* calculator_alloc(size_t len) { return run_new(len); }

void calculator_free_bytes(uint8_t* ptr, size_t len) {
    if (len != 0) run_free(ptr);
}

// A literal message, for calculator_error_set.
#define MSG(s) (const uint8_t*)(s), sizeof(s) - 1

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
    case -1:
        return 1;  // this producer counts tokens and allocations
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
    return (int32_t)((uint32_t)a + (uint32_t)b);
}

int32_t calculator_calculator_divide(int32_t a, int32_t b, calculator_error* out_err) {
    if (b == 0) {
        calculator_error_set(out_err, calculator_calculator_CalcError_DivisionByZero,
                             MSG("division by zero"));
        return 0;
    }
    if (a == INT32_MIN && b == -1) return INT32_MIN;  // wraps, like the Rust sample
    return a / b;
}

// Strings arrive as borrowed (ptr, len) and return as an owned run the
// consumer releases with calculator_free_bytes; NULL + 0 is the empty string.
const uint8_t* calculator_calculator_greet(const uint8_t* name_ptr, size_t name_len,
                                           size_t* out_len, calculator_error* out_err) {
    static const char prefix[] = "Hello, ";
    *out_len = 0;
    if (name_ptr == NULL && name_len != 0) {
        calculator_error_set(out_err, -3, MSG("name is null or invalid"));
        return NULL;
    }
    size_t n = (sizeof prefix - 1) + name_len + 1;
    uint8_t* run = run_new(n);
    if (run == NULL) {
        calculator_error_set(out_err, -1, MSG("out of memory"));
        return NULL;
    }
    memcpy(run, prefix, sizeof prefix - 1);
    if (name_len != 0) memcpy(run + sizeof prefix - 1, name_ptr, name_len);
    run[n - 1] = '!';
    *out_len = n;
    return run;
}

static int is_space(uint8_t c) { return c == ' ' || (c >= '\t' && c <= '\r'); }

// A domain error with fields: the code, a message, and the fields encoded
// with the value-buffer helper as the payload.
int32_t calculator_calculator_parse(const uint8_t* text_ptr, size_t text_len,
                                    calculator_error* out_err) {
    if (text_ptr == NULL && text_len != 0) {
        calculator_error_set(out_err, -3, MSG("text is null or invalid"));
        return 0;
    }
    size_t lo = 0, hi = text_len;
    while (lo < hi && is_space(text_ptr[lo])) lo++;
    while (hi > lo && is_space(text_ptr[hi - 1])) hi--;
    int negative = lo < hi && text_ptr[lo] == '-';
    size_t i = lo + (lo < hi && (text_ptr[lo] == '-' || text_ptr[lo] == '+'));
    int64_t value = 0;
    int ok = i < hi;
    for (; ok && i < hi; i++) {
        if (text_ptr[i] < '0' || text_ptr[i] > '9') ok = 0;
        else value = value * 10 + (text_ptr[i] - '0');
        if (value > (int64_t)INT32_MAX + 1) ok = 0;
    }
    if (ok && (negative ? -value : value) >= INT32_MIN && (negative ? -value : value) <= INT32_MAX) {
        return (int32_t)(negative ? -value : value);
    }
    static const char prefix[] = "not a number: ";
    size_t n = sizeof prefix - 1 + text_len;
    char* message = (char*)malloc(n + 1);
    if (message != NULL) {
        memcpy(message, prefix, sizeof prefix - 1);
        if (text_len != 0) memcpy(message + sizeof prefix - 1, text_ptr, text_len);
    }
    calculator_error_set(out_err, calculator_calculator_ParseError_NotANumber,
                         (const uint8_t*)message, message != NULL ? n : 0);
    free(message);
    calculator_calculator_ParseError_NotANumber_payload payload;
    payload.text.ptr = (const char*)text_ptr;
    payload.text.len = text_len;
    calculator_writer w;
    memset(&w, 0, sizeof w);
    calculator_calculator_ParseError_NotANumber_payload_write(&w, &payload);
    if (!w.failed) calculator_error_set_payload(out_err, w.ptr, w.len);
    calculator_writer_free(&w);
    return 0;
}

// `throws: any`: a failure is code -1 and a message, with no payload.
double calculator_calculator_sqrt(double x, calculator_error* out_err) {
    if (x < 0.0) {
        char message[64];
        int n = snprintf(message, sizeof message, "cannot take the square root of %g", x);
        calculator_error_set(out_err, -1, (const uint8_t*)message, n > 0 ? (size_t)n : 0);
        return 0.0;
    }
    return sqrt(x);
}

// A typed array arrives borrowed and must be aligned for its element type;
// an optional scalar returns as `bool` (present) plus the out slot.
bool calculator_calculator_mean(const double* values_ptr, size_t values_len, double* out_value,
                                calculator_error* out_err) {
    *out_value = 0.0;
    if ((values_ptr == NULL && values_len != 0) ||
        (uintptr_t)values_ptr % _Alignof(double) != 0) {
        calculator_error_set(out_err, -3, MSG("values: the array is null or misaligned"));
        return false;
    }
    if (values_len == 0) return false;
    double sum = 0.0;
    for (size_t i = 0; i < values_len; i++) sum += values_ptr[i];
    *out_value = sum / (double)values_len;
    return true;
}

// A returned typed array is a run of `count * sizeof(T)` bytes; `out_len`
// is the element count.
int32_t* calculator_calculator_running_total(const int32_t* values_ptr, size_t values_len,
                                             size_t* out_len, calculator_error* out_err) {
    *out_len = 0;
    if ((values_ptr == NULL && values_len != 0) ||
        (uintptr_t)values_ptr % _Alignof(int32_t) != 0) {
        calculator_error_set(out_err, -3, MSG("values: the array is null or misaligned"));
        return NULL;
    }
    if (values_len == 0) return NULL;
    int32_t* run = (int32_t*)(void*)run_new(values_len * sizeof(int32_t));
    if (run == NULL) {
        calculator_error_set(out_err, -1, MSG("out of memory"));
        return NULL;
    }
    uint32_t total = 0;
    for (size_t i = 0; i < values_len; i++) {
        total += (uint32_t)values_ptr[i];
        run[i] = (int32_t)total;
    }
    *out_len = values_len;
    return run;
}
