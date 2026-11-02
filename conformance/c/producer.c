// Producer-side conformance: a C library that *implements* the generated
// calculator header (C ABI revision 6) instead of consuming a prebuilt one.
//
// A hand-written producer exports three groups of symbols, all prefixed with
// the header's prefix (`calculator`):
//
//   * the API's own functions (`calculator_add`, ...);
//   * the runtime surface every consumer relies on: the ABI revision, the
//     error helpers, `alloc` and `run_free` for runs, cancel tokens, and the
//     debug leak counter;
//   * one contract function per declaration, which the header's
//     CALCULATOR_DEFINE_CONTRACT defines, so a consumer generated from a
//     different contract refuses to load and names what changed.
//
// Every API call overwrites `out_err` without reading it: a cleared error
// on success, a code and message (and payload) on failure.
//
// The harness builds this file with -fvisibility=hidden and checks with `nm`
// that every symbol is still exported (the header's CALCULATOR_API macro
// marks each prototype), then runs producer_check.c against it.
#define CALCULATOR_BUILD
#include <math.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "calculator_buffer.h"

CALCULATOR_DEFINE_CONTRACT

// Live runs (returned, consumer-allocated, and error messages and payloads)
// and cancel tokens, for calculator_debug_live.
static atomic_uint_fast64_t live_allocations;
static atomic_uint_fast64_t live_tokens;

// ── runs ───────────────────────────────────────────────────────────────────

// Every run this library hands out, and every run a consumer allocates with
// calculator_alloc, is one 8-aligned block (malloc's alignment is at least
// 8 on every supported platform), so calculator_run_free releases both.
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

static void run_release(const uint8_t* run) {
    if (run != NULL) {
        atomic_fetch_sub(&live_allocations, 1);
        free((void*)run);
    }
}

static const calculator_run EMPTY = {NULL, 0};

// ── runtime ────────────────────────────────────────────────────────────────

uint32_t calculator_abi_version(void) { return CALCULATOR_ABI_VERSION; }

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
    run_release(err->payload_ptr);
    err->payload_ptr = NULL;
    err->payload_len = 0;
    if (ptr == NULL || len == 0) return;
    err->payload_ptr = run_copy(ptr, len);
    err->payload_len = err->payload_ptr != NULL ? len : 0;
}

void calculator_error_clear(calculator_error* err) {
    if (err == NULL) return;
    run_release(err->message_ptr);
    run_release(err->payload_ptr);
    memset(err, 0, sizeof *err);
}

void calculator_error_free(calculator_error* err) {
    calculator_error_clear(err);
    free(err);
}

uint8_t* calculator_alloc(size_t len) { return run_new(len); }

void calculator_run_free(calculator_run run) { run_release(run.ptr); }

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
        return 1;  // this producer counts tokens and runs
    case 3:
        return atomic_load(&live_tokens);
    case 4:
        return atomic_load(&live_allocations);
    default:
        return 0;
    }
}

// ── call helpers ───────────────────────────────────────────────────────────

// Success: overwrite the slot with a cleared error, never reading it.
static void succeed(calculator_error* out_err) {
    if (out_err != NULL) memset(out_err, 0, sizeof *out_err);
}

// Failure: overwrite the slot with `code` and a copy of the message.
static void fail(calculator_error* out_err, int32_t code, const char* message, size_t len) {
    succeed(out_err);
    calculator_error_set(out_err, code, (const uint8_t*)message, len);
}

// A literal message, for fail().
#define MSG(s) (s), sizeof(s) - 1

// Whether a borrowed run is well formed: a null pointer only with length 0.
static bool run_ok(calculator_run run) { return run.ptr != NULL || run.len == 0; }

// Whether a borrowed run is a typed array of `size`-byte elements: well
// formed, a whole number of elements, and aligned for them.
static bool typed_ok(calculator_run run, size_t size) {
    return run_ok(run) && run.len % size == 0 && (uintptr_t)run.ptr % size == 0;
}

// ── API ────────────────────────────────────────────────────────────────────

int32_t calculator_add(int32_t a, int32_t b, calculator_error* out_err) {
    succeed(out_err);
    return (int32_t)((uint32_t)a + (uint32_t)b);
}

int32_t calculator_divide(int32_t a, int32_t b, calculator_error* out_err) {
    if (b == 0) {
        fail(out_err, calculator_CalcError_DivisionByZero, MSG("division by zero"));
        return 0;
    }
    succeed(out_err);
    if (a == INT32_MIN && b == -1) return INT32_MIN;  // wraps, like the Rust sample
    return a / b;
}

// A string arrives as a borrowed run of UTF-8 and returns as an owned run
// the consumer releases with calculator_run_free; the empty run is the
// empty string.
calculator_run calculator_greet(calculator_run name, calculator_error* out_err) {
    static const char prefix[] = "Hello, ";
    if (!run_ok(name) || !calculator_reader_utf8(name.ptr, name.len)) {
        fail(out_err, -3, MSG("name: not a valid UTF-8 string"));
        return EMPTY;
    }
    size_t n = (sizeof prefix - 1) + name.len + 1;
    uint8_t* run = run_new(n);
    if (run == NULL) {
        fail(out_err, -1, MSG("out of memory"));
        return EMPTY;
    }
    memcpy(run, prefix, sizeof prefix - 1);
    if (name.len != 0) memcpy(run + sizeof prefix - 1, name.ptr, name.len);
    run[n - 1] = '!';
    succeed(out_err);
    calculator_run out = {run, n};
    return out;
}

static int is_space(uint8_t c) { return c == ' ' || (c >= '\t' && c <= '\r'); }

// A domain error with fields: the code, a message, and the fields encoded
// with the value-buffer helper as the payload.
int32_t calculator_parse(calculator_run text, calculator_error* out_err) {
    if (!run_ok(text) || !calculator_reader_utf8(text.ptr, text.len)) {
        fail(out_err, -3, MSG("text: not a valid UTF-8 string"));
        return 0;
    }
    const uint8_t* p = text.ptr;
    size_t lo = 0, hi = text.len;
    while (lo < hi && is_space(p[lo])) lo++;
    while (hi > lo && is_space(p[hi - 1])) hi--;
    int negative = lo < hi && p[lo] == '-';
    size_t i = lo + (lo < hi && (p[lo] == '-' || p[lo] == '+'));
    int64_t value = 0;
    int ok = i < hi;
    for (; ok && i < hi; i++) {
        if (p[i] < '0' || p[i] > '9') ok = 0;
        else value = value * 10 + (p[i] - '0');
        if (value > (int64_t)INT32_MAX + 1) ok = 0;
    }
    if (ok && (negative ? -value : value) >= INT32_MIN && (negative ? -value : value) <= INT32_MAX) {
        succeed(out_err);
        return (int32_t)(negative ? -value : value);
    }
    static const char prefix[] = "not a number: ";
    size_t n = sizeof prefix - 1 + text.len;
    char* message = (char*)malloc(n + 1);
    if (message != NULL) {
        memcpy(message, prefix, sizeof prefix - 1);
        if (text.len != 0) memcpy(message + sizeof prefix - 1, text.ptr, text.len);
    }
    fail(out_err, calculator_ParseError_NotANumber, message, message != NULL ? n : 0);
    free(message);
    calculator_ParseError_NotANumber_payload payload;
    payload.text.ptr = (const char*)text.ptr;
    payload.text.len = text.len;
    calculator_writer w;
    memset(&w, 0, sizeof w);
    calculator_ParseError_NotANumber_payload_write(&w, &payload);
    if (!w.failed) calculator_error_set_payload(out_err, w.ptr, w.len);
    calculator_writer_free(&w);
    return 0;
}

// `throws: any`: a failure is code -1 and a message, with no payload.
double calculator_sqrt(double x, calculator_error* out_err) {
    if (x < 0.0) {
        char message[64];
        int n = snprintf(message, sizeof message, "cannot take the square root of %g", x);
        fail(out_err, -1, message, n > 0 ? (size_t)n : 0);
        return 0.0;
    }
    succeed(out_err);
    return sqrt(x);
}

// A typed array arrives as a borrowed run (`len` in bytes) aligned for its
// element type; an optional scalar returns as an opt struct whose `value` is
// 0 when absent.
calculator_opt_f64 calculator_mean(calculator_run values, calculator_error* out_err) {
    calculator_opt_f64 none = {false, 0.0};
    if (!typed_ok(values, sizeof(double))) {
        fail(out_err, -3, MSG("values: not a well-formed [f64]"));
        return none;
    }
    succeed(out_err);
    size_t n = values.len / sizeof(double);
    if (n == 0) return none;
    const double* xs = (const double*)(const void*)values.ptr;
    double sum = 0.0;
    for (size_t i = 0; i < n; i++) sum += xs[i];
    calculator_opt_f64 mean = {true, sum / (double)n};
    return mean;
}

// A returned typed array is a run of `count * sizeof(T)` bytes.
calculator_run calculator_running_total(calculator_run values, calculator_error* out_err) {
    if (!typed_ok(values, sizeof(int32_t))) {
        fail(out_err, -3, MSG("values: not a well-formed [i32]"));
        return EMPTY;
    }
    size_t n = values.len / sizeof(int32_t);
    if (n == 0) {
        succeed(out_err);
        return EMPTY;
    }
    int32_t* run = (int32_t*)(void*)run_new(values.len);
    if (run == NULL) {
        fail(out_err, -1, MSG("out of memory"));
        return EMPTY;
    }
    const int32_t* xs = (const int32_t*)(const void*)values.ptr;
    uint32_t total = 0;
    for (size_t i = 0; i < n; i++) {
        total += (uint32_t)xs[i];
        run[i] = (int32_t)total;
    }
    succeed(out_err);
    calculator_run out = {(const uint8_t*)run, values.len};
    return out;
}
