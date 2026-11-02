// Shared helpers for the C conformance consumers (C ABI revision 6):
// always-on assertions, run shorthands (HARNESS_RUNS), error-message
// comparison, and the leak check every consumer runs before exiting.
#ifndef CONFORMANCE_HARNESS_H
#define CONFORMANCE_HARNESS_H

#undef NDEBUG
#ifndef _POSIX_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#endif
#include <assert.h>
#include <time.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static inline void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

// `true` when the `len` bytes at `ptr` spell the C string `s` exactly.
static inline int bytes_eq(const uint8_t* ptr, size_t len, const char* s) {
    size_t n = strlen(s);
    return len == n && (n == 0 || memcmp(ptr, s, n) == 0);
}

// The 64-bit FNV-1a hash of a C string: a declaration's contract id is the
// hash of its dotted path, and the value its contract function returns is
// the hash of its canonical signature.
static inline uint64_t fnv1a64(const char* s) {
    uint64_t h = 0xcbf29ce484222325ull;
    for (; *s != '\0'; s++) {
        h ^= (uint8_t)*s;
        h *= 0x100000001b3ull;
    }
    return h;
}

// `true` when an error struct's length-delimited UTF-8 message (not
// NUL-terminated; NULL with length 0 is the empty message) spells `s`.
#define MSG_EQ(err, s) bytes_eq((err).message_ptr, (err).message_len, (s))

// Define the run shorthands for a library prefix `p`: `str_run("text")`
// borrows a C string, `bytes_run(ptr, len)` borrows bytes, `run_eq(run,
// "text")` compares a run's bytes, and `run_take_eq(run, "text")` compares
// a returned run and releases it.
#define HARNESS_UNUSED __attribute__((unused))
#define HARNESS_RUNS(p)                                                    \
    HARNESS_UNUSED static inline p##_run str_run(const char* s) {         \
        p##_run r = {(const uint8_t*)s, s ? strlen(s) : 0};               \
        return r;                                                          \
    }                                                                      \
    HARNESS_UNUSED static inline p##_run bytes_run(const void* ptr,       \
                                                    size_t len) {          \
        p##_run r = {(const uint8_t*)ptr, len};                            \
        return r;                                                          \
    }                                                                      \
    HARNESS_UNUSED static inline int run_eq(p##_run r, const char* s) {   \
        return bytes_eq(r.ptr, r.len, s);                                  \
    }                                                                      \
    HARNESS_UNUSED static inline int run_take_eq(p##_run r,               \
                                                 const char* s) {          \
        int eq = bytes_eq(r.ptr, r.len, s);                                \
        p##_run_free(r);                                                   \
        return eq;                                                         \
    }

// Assert that every leak counter of a producer built with `leak-check` is
// zero: 0 objects, 1 callbacks, 2 iterators, 3 cancel tokens, 4 runs. A
// producer worker thread may still be unwinding a finished async call
// (dropping its future and token reference just after the completion
// fired), so the counters get up to two seconds to settle.
#define ASSERT_NO_LEAKS(debug_live)                                          \
    do {                                                                     \
        static const char* const kinds[5] = {"objects", "callbacks",          \
                                             "iterators", "cancel tokens",   \
                                             "runs"};                 \
        for (int wait = 0; wait < 2000; wait++) {                            \
            uint64_t total = 0;                                              \
            for (int32_t kind = 0; kind <= 4; kind++) total += debug_live(kind); \
            if (total == 0) break;                                           \
            sleep_ms(1);                                                     \
        }                                                                    \
        for (int32_t kind = 0; kind <= 4; kind++) {                          \
            uint64_t live = debug_live(kind);                                \
            if (live != 0) {                                                 \
                fprintf(stderr, "leak: %llu live %s\n",                      \
                        (unsigned long long)live, kinds[kind]);              \
                abort();                                                     \
            }                                                                \
        }                                                                    \
    } while (0)

#endif  // CONFORMANCE_HARNESS_H
