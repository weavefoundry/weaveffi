// Conformance consumer: async-demo sample, C++ target.
//
// Drives the std::future-backed async surface of the generated header-only
// wrapper end to end: `weaveffi::tasks::run_task` settled from the producer's
// worker thread with a TaskResult struct decoded from a value buffer, the
// typed InvalidNameError (extending TaskError extending WeaveFFIError)
// rethrown by future::get for an empty name, the buffered list-of-records
// round trip through `run_batch`, the direct-scalar `run_n_tasks`, the sync
// `cancel_task`, and `active_callbacks` settling to zero once every task body
// has completed. Exits 0 on success; aborts (non-zero) on any failed
// assertion.

#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "weaveffi.hpp"

int main() {
    // Async record return: future::get blocks until the worker-thread
    // callback delivers the decoded TaskResult.
    weaveffi::TaskResult result = weaveffi::tasks::run_task("alpha").get();
    assert(result.id > 0);
    assert(result.value == "completed: alpha");
    assert(result.success);

    // Typed async error: the empty name settles the future with the typed
    // exception, rethrown on get().
    bool threw = false;
    try {
        weaveffi::tasks::run_task("").get();
    } catch (const weaveffi::InvalidNameError& e) {
        threw = true;
        assert(e.code() == 1);
        assert(dynamic_cast<const weaveffi::TaskError*>(&e) != nullptr);
        assert(dynamic_cast<const weaveffi::WeaveFFIError*>(&e) != nullptr);
    }
    assert(threw && "expected InvalidNameError for empty name");

    // Buffered list-of-records both ways.
    std::vector<weaveffi::TaskResult> batch =
        weaveffi::tasks::run_batch({"a", "b", "c"}).get();
    assert(batch.size() == 3);
    const char* expected[3] = {"completed: a", "completed: b", "completed: c"};
    for (size_t i = 0; i < 3; i++) {
        assert(batch[i].value == expected[i]);
        assert(batch[i].success);
    }

    // Direct scalar through the async callback.
    assert(weaveffi::tasks::run_n_tasks(7).get() == 7);

    // Sync functions beside the async ones.
    assert(!weaveffi::tasks::cancel_task(1));

    // Every spawned task body has completed by the time its callback fires.
    assert(weaveffi::tasks::active_callbacks() == 0);

    std::printf("cpp async-demo conformance: OK\n");
    return 0;
}
