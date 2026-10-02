// Shared helpers for the C conformance consumers: always-on assertions,
// string-slot shorthands for the ABI's (ptr, len) strings, and the leak
// check every consumer runs before exiting.
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

// Expand a NUL-terminated C string into the two slots of a string
// parameter: `const uint8_t* {name}_ptr, size_t {name}_len`.
#define STR(s) (const uint8_t*)(s), strlen(s)

// Expand a byte array into a bytes parameter's (ptr, len) slots.
#define BYTES(a) (const uint8_t*)(a), sizeof(a)

static inline void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

// `true` when the `len` bytes at `ptr` spell the C string `s` exactly.
static inline int bytes_eq(const uint8_t* ptr, size_t len, const char* s) {
    size_t n = strlen(s);
    return len == n && (n == 0 || memcmp(ptr, s, n) == 0);
}

// Assert that every leak counter of a producer built with `leak-check` is
// zero: 0 objects, 1 callbacks, 2 iterators, 3 cancel tokens, 4 returned
// allocations. A producer worker thread may still be unwinding a finished
// async call (dropping its future and token reference just after the
// completion fired), so the counters get up to two seconds to settle.
#define ASSERT_NO_LEAKS(debug_live)                                          \
    do {                                                                     \
        static const char* const kinds[5] = {"objects", "callbacks",          \
                                             "iterators", "cancel tokens",   \
                                             "allocations"};                 \
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
