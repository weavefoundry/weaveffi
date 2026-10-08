// Shared helpers for the C++ conformance consumers.
//
// CHECK aborts the consumer with a message on a failed assertion (unlike
// assert, it isn't compiled out under NDEBUG). expect_throw runs a call that
// must throw exactly `E` (or a subclass) and returns the caught exception.
// check_no_leaks confirms the producer counts (`debug_live(-1) == 1`) and
// polls every `{prefix}_debug_live` counter until it reads zero: async
// completions release their last producer-side references (the task, the
// cancel token reference) just after the consumer's future is ready, so the
// counters settle shortly after the last call returns.

#pragma once

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <optional>
#include <thread>
#include <typeinfo>

#define CHECK(cond)                                                                        \
    do {                                                                                   \
        if (!(cond)) {                                                                     \
            std::fprintf(stderr, "%s:%d: check failed: %s\n", __FILE__, __LINE__, #cond); \
            std::exit(1);                                                                  \
        }                                                                                  \
    } while (0)

// Run `fn`, which must throw an `E`; return a copy of what it threw. Any
// other outcome fails with `what` naming the call.
template <typename E, typename F>
E expect_throw(F&& fn, const char* what) {
    try {
        fn();
    } catch (const E& e) {
        return e;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "%s: threw %s (%s), not the expected exception\n", what,
                     typeid(e).name(), e.what());
        std::exit(1);
    }
    std::fprintf(stderr, "%s: returned instead of throwing\n", what);
    std::exit(1);
}

// Assert that the producer counts resources and that `debug_live(kind)`
// settles to zero for every kind: 0 objects, 1 callbacks, 2 iterators,
// 3 cancel tokens, 4 byte runs.
inline void check_no_leaks(uint64_t (*debug_live)(int32_t), const char* sample) {
    static const char* const kinds[] = {"objects", "callbacks", "iterators", "cancel tokens",
                                        "byte runs"};
    if (debug_live(-1) != 1) {
        std::fprintf(stderr, "cpp/%s: the producer doesn't count live resources\n", sample);
        std::exit(1);
    }
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
