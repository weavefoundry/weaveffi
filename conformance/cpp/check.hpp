// Shared helpers for the C++ conformance consumers.
//
// CHECK aborts the consumer with a message on a failed assertion (unlike
// assert, it isn't compiled out under NDEBUG). check_no_leaks polls a
// producer's `{prefix}_debug_live` counters until every kind reads zero:
// async completions release their last producer-side references (the task,
// the cancel token reference) just after the consumer's future is ready, so
// the counters settle shortly after the last call returns.

#pragma once

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <thread>

#define CHECK(cond)                                                                    \
    do {                                                                               \
        if (!(cond)) {                                                                 \
            std::fprintf(stderr, "%s:%d: check failed: %s\n", __FILE__, __LINE__, #cond); \
            std::exit(1);                                                              \
        }                                                                              \
    } while (0)

// Assert that `debug_live(kind)` settles to zero for every kind: 0 objects,
// 1 callbacks, 2 iterators, 3 cancel tokens, 4 returned allocations.
inline void check_no_leaks(uint64_t (*debug_live)(int32_t), const char* sample) {
    static const char* const kinds[] = {"objects", "callbacks", "iterators", "cancel tokens",
                                        "allocations"};
    for (int32_t kind = 0; kind <= 4; ++kind) {
        uint64_t live = debug_live(kind);
        for (int i = 0; live != 0 && i < 200; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
            live = debug_live(kind);
        }
        if (live != 0) {
            std::fprintf(stderr, "cpp/%s: %llu live %s at exit\n", sample,
                         static_cast<unsigned long long>(live), kinds[kind]);
            std::exit(1);
        }
    }
}
