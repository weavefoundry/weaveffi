// Async stress test for the C++ async lifecycle.
//
// Loads the async-demo cdylib (path in ASYNC_DEMO_LIB) and spawns 1000
// concurrent calls to weaveffi_tasks_run_n_tasks_async via the C ABI directly
// (so the test doesn't depend on the generator output).
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback receives the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//
// A leak in the C++ wrapper (heap-allocated std::promise not freed) would
// not be visible from the C side, but if the closure heap allocation were
// not freed exactly once the test would crash on double-free or leak
// memory; the trailing active_callbacks==0 check catches the worker leak.
//
// Prints "OK" and exits 0 on success.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <dlfcn.h>
#include <mutex>
#include <thread>
#include <vector>

extern "C" {
struct weaveffi_error {
    int32_t code;
    const char* message;
};
}

constexpr int N_TASKS = 1000;

using run_n_tasks_cb_t = void (*)(void* context, weaveffi_error* err, int32_t result);
using run_n_tasks_async_t = void (*)(int32_t n, run_n_tasks_cb_t cb, void* context);
using active_callbacks_t = int64_t (*)(weaveffi_error* err);

struct Slot {
    int idx;
    std::atomic<int> result{-1};
};

static std::atomic<int> remaining{N_TASKS};
static std::mutex done_mu;
static std::condition_variable done_cv;

extern "C" void on_done(void* context, weaveffi_error* err, int32_t result) {
    (void)err;
    auto* slot = static_cast<Slot*>(context);
    slot->result.store(result, std::memory_order_release);
    if (remaining.fetch_sub(1, std::memory_order_acq_rel) == 1) {
        std::lock_guard<std::mutex> lock(done_mu);
        done_cv.notify_all();
    }
}

int main() {
    const char* lib_path = std::getenv("ASYNC_DEMO_LIB");
    if (!lib_path) {
        std::fprintf(stderr, "ASYNC_DEMO_LIB not set\n");
        return 1;
    }
    void* lib = dlopen(lib_path, RTLD_NOW | RTLD_GLOBAL);
    if (!lib) {
        std::fprintf(stderr, "dlopen(%s): %s\n", lib_path, dlerror());
        return 1;
    }

    auto run_n_tasks_async = reinterpret_cast<run_n_tasks_async_t>(
        dlsym(lib, "weaveffi_tasks_run_n_tasks_async"));
    auto active_callbacks = reinterpret_cast<active_callbacks_t>(
        dlsym(lib, "weaveffi_tasks_active_callbacks"));
    if (!run_n_tasks_async || !active_callbacks) {
        std::fprintf(stderr, "dlsym failed: %s\n", dlerror());
        return 1;
    }

    std::vector<Slot> slots(N_TASKS);
    for (int i = 0; i < N_TASKS; ++i) {
        slots[i].idx = i;
        run_n_tasks_async(i, on_done, &slots[i]);
    }

    {
        std::unique_lock<std::mutex> lock(done_mu);
        if (!done_cv.wait_for(lock, std::chrono::seconds(30),
                              [] { return remaining.load() == 0; })) {
            std::fprintf(stderr, "timeout waiting for callbacks; %d left\n",
                         remaining.load());
            return 1;
        }
    }

    for (int i = 0; i < N_TASKS; ++i) {
        int v = slots[i].result.load(std::memory_order_acquire);
        if (v != i) {
            std::fprintf(stderr, "result[%d] = %d, expected %d\n", i, v, i);
            return 1;
        }
    }

    weaveffi_error err{0, nullptr};
    int64_t active = active_callbacks(&err);
    if (err.code != 0 || active != 0) {
        std::fprintf(stderr, "active_callbacks = %lld (expected 0)\n",
                     static_cast<long long>(active));
        return 1;
    }

    std::printf("OK\n");
    return 0;
}
