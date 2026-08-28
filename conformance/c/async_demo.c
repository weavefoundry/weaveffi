// Conformance consumer: async-demo sample, C target.
//
// Includes the *generated* C header and links the async_demo cdylib,
// exercising the raw async launcher convention: each `*_async` symbol takes a
// completion callback plus a context pointer and fires it once from the
// producer's worker thread, with buffered results (the TaskResult record and
// the list-of-records batch) borrowed for the duration of the callback and
// direct scalars passed by value. Also covers the typed error code delivered
// through the callback's error slot (InvalidName == 1), the plain sync
// functions beside the async ones, and active_callbacks settling to zero.
// Completion arrives on the producer's worker thread, so each wait polls an
// atomic flag. Exits 0 on success; aborts (non-zero) on any failed assertion.

#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "weaveffi.h"
#include "wvbuf.h"

// A decoded TaskResult record: id, value, success.
typedef struct {
    int64_t id;
    char* value;
    int success;
} task_result_t;

static void read_task_result(wv_reader* r, task_result_t* t) {
    t->id = wv_get_i64(r);
    t->value = wv_get_str(r);
    t->success = wv_get_bool(r);
}

// Poll the completion flag; the callback fires on the producer's worker
// thread, so the main thread spins until it lands.
static void wait_done(atomic_int* done) {
    while (!atomic_load(done)) {
    }
}

// --- run_task("alpha"): buffered record result, decoded in the callback. ---
static atomic_int g_task_done = 0;
static int32_t g_task_err = -1;
static task_result_t g_task_result;

static void on_task_done(void* context, weaveffi_error* err,
                         const uint8_t* result_ptr, size_t result_len) {
    (void)context;
    g_task_err = err ? err->code : 0;
    if (g_task_err == 0) {
        wv_reader r;
        wv_r_init(&r, result_ptr, result_len);
        read_task_result(&r, &g_task_result);
        wv_r_expect_end(&r);
    }
    atomic_store(&g_task_done, 1);
}

// --- run_task(""): the typed error code lands in the callback's err slot. ---
static atomic_int g_err_done = 0;
static int32_t g_err_code = -1;

static void on_task_err(void* context, weaveffi_error* err,
                        const uint8_t* result_ptr, size_t result_len) {
    (void)context;
    (void)result_ptr;
    (void)result_len;
    g_err_code = err ? err->code : 0;
    atomic_store(&g_err_done, 1);
}

// --- run_batch: buffered list-of-records result. ---
static atomic_int g_batch_done = 0;
static int32_t g_batch_err = -1;
static size_t g_batch_count = 0;
static task_result_t g_batch[3];

static void on_batch_done(void* context, weaveffi_error* err,
                          const uint8_t* result_ptr, size_t result_len) {
    (void)context;
    g_batch_err = err ? err->code : 0;
    if (g_batch_err == 0) {
        wv_reader r;
        wv_r_init(&r, result_ptr, result_len);
        g_batch_count = wv_get_u32(&r);
        for (size_t i = 0; i < g_batch_count && i < 3; i++) {
            read_task_result(&r, &g_batch[i]);
        }
        wv_r_expect_end(&r);
    }
    atomic_store(&g_batch_done, 1);
}

// --- run_n_tasks: direct scalar result. ---
static atomic_int g_n_done = 0;
static int32_t g_n_err = -1;
static int32_t g_n_result = -1;

static void on_n_done(void* context, weaveffi_error* err, int32_t result) {
    (void)context;
    g_n_err = err ? err->code : 0;
    g_n_result = result;
    atomic_store(&g_n_done, 1);
}

int main(void) {
    weaveffi_error err = {0};

    // Async record return: the callback borrows the encoded TaskResult.
    weaveffi_tasks_run_task_async("alpha", on_task_done, NULL);
    wait_done(&g_task_done);
    assert(g_task_err == 0);
    assert(g_task_result.id > 0);
    assert(strcmp(g_task_result.value, "completed: alpha") == 0);
    assert(g_task_result.success == 1);
    free(g_task_result.value);

    // Typed async error: the empty name reports InvalidName (code 1).
    weaveffi_tasks_run_task_async("", on_task_err, NULL);
    wait_done(&g_err_done);
    assert(g_err_code == 1);

    // Buffered list-of-records both ways: encode ["a", "b", "c"], decode
    // three results.
    wv_writer names;
    wv_w_init(&names);
    wv_put_u32(&names, 3);
    wv_put_str(&names, "a");
    wv_put_str(&names, "b");
    wv_put_str(&names, "c");
    weaveffi_tasks_run_batch_async(names.buf, names.len, on_batch_done, NULL);
    wait_done(&g_batch_done);
    wv_w_free(&names);
    assert(g_batch_err == 0);
    assert(g_batch_count == 3);
    const char* expected[3] = {"completed: a", "completed: b", "completed: c"};
    for (size_t i = 0; i < 3; i++) {
        assert(strcmp(g_batch[i].value, expected[i]) == 0);
        assert(g_batch[i].success == 1);
        free(g_batch[i].value);
    }

    // Direct scalar through the async callback.
    weaveffi_tasks_run_n_tasks_async(7, on_n_done, NULL);
    wait_done(&g_n_done);
    assert(g_n_err == 0);
    assert(g_n_result == 7);

    // Sync functions beside the async ones.
    assert(weaveffi_tasks_cancel_task(1, &err) == false);
    assert(err.code == 0);

    // Every spawned task body has completed by the time its callback fires.
    assert(weaveffi_tasks_active_callbacks(&err) == 0);
    assert(err.code == 0);

    printf("c async-demo conformance: OK\n");
    return 0;
}
