// Conformance consumer: async-demo sample, C++ target.
//
// Drives the std::future-backed async surface end to end: `run_task`
// settled from a producer thread with a decoded TaskResult record, the typed
// InvalidNameError (extending TaskError extending Error) rethrown by
// future::get, the buffered list-of-records round trip through `run_batch`,
// the direct-scalar `run_n_tasks`, and cancellation of the cancellable
// `wait` through an RAII CancelToken: cancelling an in-flight call settles
// its future with Cancelled (code -5) long before its timeout, a token
// destroyed mid-flight leaves the call running, and omitting the token runs
// to completion. Ends by asserting no task body is still active and the
// producer's leak counters are all zero.

#include <chrono>
#include <cstdio>
#include <future>
#include <string>
#include <thread>
#include <vector>

#include "async_demo.hpp"
#include "check.hpp"

namespace ad = ::async_demo;

static void run() {
    ad::check_library();

    ad::TaskResult result = ad::tasks::run_task("alpha").get();
    CHECK(result.id > 0);
    CHECK(result.value == "completed: alpha");
    CHECK(result.success);

    bool threw = false;
    try {
        ad::tasks::run_task("").get();
    } catch (const ad::InvalidNameError& e) {
        threw = e.code() == 1;
        CHECK(dynamic_cast<const ad::TaskError*>(&e) != nullptr);
        CHECK(dynamic_cast<const ad::Error*>(&e) != nullptr);
    }
    CHECK(threw);

    std::vector<ad::TaskResult> batch = ad::tasks::run_batch({"a", "b", "c"}).get();
    CHECK(batch.size() == 3);
    const char* expected[3] = {"completed: a", "completed: b", "completed: c"};
    for (size_t i = 0; i < 3; i++) {
        CHECK(batch[i].value == expected[i]);
        CHECK(batch[i].success);
    }
    CHECK(ad::tasks::run_batch({}).get().empty());

    CHECK(ad::tasks::run_n_tasks(7).get() == 7);

    // Many concurrent calls, each settled exactly once.
    {
        std::vector<std::future<int32_t>> pending;
        for (int32_t i = 0; i < 64; i++) pending.push_back(ad::tasks::run_n_tasks(i));
        for (int32_t i = 0; i < 64; i++) CHECK(pending[i].get() == i);
    }

    // No token: the call runs to its timeout.
    CHECK(ad::tasks::wait(20).get() == 20);

    // Cancelling an in-flight call settles it with Cancelled right away.
    {
        ad::CancelToken token;
        auto start = std::chrono::steady_clock::now();
        std::future<int64_t> pending = ad::tasks::wait(30000, token);
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        CHECK(pending.wait_for(std::chrono::seconds(0)) == std::future_status::timeout);
        token.cancel();
        CHECK(token.is_cancelled());
        bool cancelled = false;
        try {
            pending.get();
        } catch (const ad::Cancelled& e) {
            cancelled = e.code() == -5;
        }
        CHECK(cancelled);
        CHECK(std::chrono::steady_clock::now() - start < std::chrono::seconds(10));
    }

    // A token cancelled before the call starts cancels it too, and one token
    // can cancel several calls.
    {
        ad::CancelToken token;
        std::future<int64_t> first = ad::tasks::wait(30000, token);
        std::future<int64_t> second = ad::tasks::wait(30000, token);
        token.cancel();
        int cancelled = 0;
        for (auto* f : {&first, &second}) {
            try {
                f->get();
            } catch (const ad::Cancelled&) {
                cancelled++;
            }
        }
        CHECK(cancelled == 2);
        std::future<int64_t> late = ad::tasks::wait(30000, token);
        bool caught = false;
        try {
            late.get();
        } catch (const ad::Cancelled&) {
            caught = true;
        }
        CHECK(caught);
    }

    // Destroying the consumer's token mid-flight releases only its own
    // reference: the call keeps running and completes normally.
    {
        std::future<int64_t> pending;
        {
            ad::CancelToken token;
            pending = ad::tasks::wait(50, token);
        }
        CHECK(pending.get() == 50);
    }

    // A moved-from token is empty and cancelling it is a no-op.
    {
        ad::CancelToken token;
        ad::CancelToken moved = std::move(token);
        token.cancel();
        CHECK(!moved.is_cancelled());
        CHECK(ad::tasks::wait(1, moved).get() == 1);
    }

    // Every spawned task body has finished, including the cancelled ones,
    // whose futures the runtime dropped.
    for (int i = 0; i < 200 && ad::tasks::active_callbacks() != 0; i++) {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    CHECK(ad::tasks::active_callbacks() == 0);
}

int main() {
    run();
    check_no_leaks(async_demo_debug_live, "async-demo");
    std::printf("cpp/async-demo: OK\n");
    return 0;
}
