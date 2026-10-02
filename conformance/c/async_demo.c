// Conformance consumer: async-demo sample, C target (ABI revision 3).
//
// Exercises the raw async launcher convention: each async function's symbol
// takes its inputs, a completion callback, and a context pointer, returns at
// once, and fires the callback exactly once from a producer thread. Buffered
// results (the TaskResult record, the list-of-records batch) arrive as
// consumer-owned (ptr, len) runs released with async_demo_free_bytes; a
// non-null error is heap-boxed and released with async_demo_error_free.
// The cancellable `wait` takes a cancel token: cancelling it completes the
// call with code -5 long before its timeout, whether the token fires before
// or after launch, and the consumer may destroy its token reference at any
// time. Also covers many concurrent launches, the plain sync function beside
// the async ones, and the producer's leak counters settling to zero.

#include "harness.h"

#include <stdatomic.h>

#include "async_demo_buffer.h"

#define CANCELLED (-5)

// One in-flight call's completion slot; the callback runs on a producer
// thread, so the main thread polls `done`.
typedef struct {
    atomic_int done;
    atomic_int fired;  // completions observed (must end at exactly 1)
    int32_t code;
    char message[96];
    int64_t i64;
    int32_t i32;
    uint8_t* buf;  // owned result run
    size_t buf_len;
} call;

static void call_reset(call* c) {
    memset(c, 0, sizeof *c);
    atomic_init(&c->done, 0);
    atomic_init(&c->fired, 0);
}

static void record_err(call* c, async_demo_error* err) {
    c->code = err ? err->code : 0;
    if (err && err->message) snprintf(c->message, sizeof c->message, "%s", err->message);
    async_demo_error_free(err);
}

static void finish(call* c) {
    atomic_fetch_add(&c->fired, 1);
    atomic_store(&c->done, 1);
}

static void on_buffer(void* context, async_demo_error* err, const uint8_t* result_ptr,
                      size_t result_len) {
    call* c = (call*)context;
    record_err(c, err);
    // The result run is ours: keep it and release it after decoding.
    c->buf = (uint8_t*)result_ptr;
    c->buf_len = result_len;
    finish(c);
}

static void on_i64(void* context, async_demo_error* err, int64_t result) {
    call* c = (call*)context;
    record_err(c, err);
    c->i64 = result;
    finish(c);
}

static void on_i32(void* context, async_demo_error* err, int32_t result) {
    call* c = (call*)context;
    record_err(c, err);
    c->i32 = result;
    finish(c);
}

// Wait for a completion, failing after `limit_ms`. Returns the elapsed time.
static long await_call(call* c, long limit_ms) {
    long waited = 0;
    while (!atomic_load(&c->done)) {
        assert(waited < limit_ms && "async call did not complete in time");
        sleep_ms(1);
        waited++;
    }
    assert(atomic_load(&c->fired) == 1);
    return waited;
}

static void run_task(void) {
    call c;
    call_reset(&c);
    async_demo_tasks_run_task(STR("alpha"), on_buffer, &c);
    await_call(&c, 5000);
    assert(c.code == 0 && c.buf != NULL);
    async_demo_tasks_TaskResult r;
    assert(async_demo_tasks_TaskResult_decode(c.buf, c.buf_len, &r));
    async_demo_free_bytes(c.buf, c.buf_len);
    assert(r.id > 0);
    assert(strcmp(r.value.ptr, "completed: alpha") == 0);
    assert(r.success);
    async_demo_tasks_TaskResult_free(&r);

    // Typed async error: the empty name reports InvalidName with its message
    // and no result run.
    call_reset(&c);
    async_demo_tasks_run_task(NULL, 0, on_buffer, &c);
    await_call(&c, 5000);
    assert(c.code == async_demo_tasks_TaskError_InvalidName);
    assert(strcmp(c.message, "task name must not be empty") == 0);
    assert(c.buf == NULL && c.buf_len == 0);
}

static void run_batch(void) {
    async_demo_str names[3] = {async_demo_str_of("a"), async_demo_str_of("b"),
                               async_demo_str_of("c")};
    async_demo_list_string in;
    in.items = names;
    in.len = 3;
    async_demo_writer w;
    memset(&w, 0, sizeof w);
    async_demo_list_string_write(&w, &in);
    call c;
    call_reset(&c);
    async_demo_tasks_run_batch(w.ptr, w.len, on_buffer, &c);
    // Inputs are lifted before the launcher returns, so the buffer can go now.
    async_demo_writer_free(&w);
    await_call(&c, 5000);
    assert(c.code == 0);
    async_demo_list_tasks_TaskResult out;
    assert(async_demo_list_tasks_TaskResult_decode(c.buf, c.buf_len, &out));
    async_demo_free_bytes(c.buf, c.buf_len);
    assert(out.len == 3);
    const char* expected[3] = {"completed: a", "completed: b", "completed: c"};
    for (size_t i = 0; i < 3; i++) {
        assert(strcmp(out.items[i].value.ptr, expected[i]) == 0);
        assert(out.items[i].success);
    }
    assert(out.items[0].id < out.items[1].id && out.items[1].id < out.items[2].id);
    async_demo_list_tasks_TaskResult_free(&out);
}

static void run_n_tasks(void) {
    // Many concurrent launches, each with its own context: every completion
    // fires exactly once with its own value.
    enum { N = 64 };
    static call calls[N];
    for (int i = 0; i < N; i++) {
        call_reset(&calls[i]);
        async_demo_tasks_run_n_tasks(i, on_i32, &calls[i]);
    }
    for (int i = 0; i < N; i++) {
        await_call(&calls[i], 5000);
        assert(calls[i].code == 0 && calls[i].i32 == i);
    }
}

static void wait_and_cancel(void) {
    call c;

    // No token: wait runs to its (short) timeout.
    call_reset(&c);
    async_demo_tasks_wait(5, NULL, on_i64, &c);
    await_call(&c, 5000);
    assert(c.code == 0 && c.i64 == 5);

    // A token that is never cancelled changes nothing.
    async_demo_cancel_token* token = async_demo_cancel_token_create();
    assert(token != NULL && !async_demo_cancel_token_is_cancelled(token));
    call_reset(&c);
    async_demo_tasks_wait(10, token, on_i64, &c);
    await_call(&c, 5000);
    assert(c.code == 0 && c.i64 == 10);
    async_demo_cancel_token_destroy(token);

    // Cancel mid-flight: a one-minute wait completes with -5 almost at once.
    // The consumer drops its token reference right after cancelling; the
    // producer holds its own until the call completes.
    token = async_demo_cancel_token_create();
    call_reset(&c);
    async_demo_tasks_wait(60000, token, on_i64, &c);
    sleep_ms(20);
    assert(!atomic_load(&c.done) && "the wait is still pending");
    async_demo_cancel_token_cancel(token);
    async_demo_cancel_token_cancel(token);  // idempotent
    assert(async_demo_cancel_token_is_cancelled(token));
    async_demo_cancel_token_destroy(token);
    long waited = await_call(&c, 5000);
    assert(waited < 5000);
    assert(c.code == CANCELLED);
    assert(c.message[0] != '\0');

    // Cancelled before launch: completes with -5 without waiting.
    token = async_demo_cancel_token_create();
    async_demo_cancel_token_cancel(token);
    call_reset(&c);
    async_demo_tasks_wait(60000, token, on_i64, &c);
    await_call(&c, 5000);
    assert(c.code == CANCELLED);
    async_demo_cancel_token_destroy(token);

    // One token cancels several in-flight calls at once.
    token = async_demo_cancel_token_create();
    call many[3];
    for (int i = 0; i < 3; i++) {
        call_reset(&many[i]);
        async_demo_tasks_wait(60000, token, on_i64, &many[i]);
    }
    async_demo_cancel_token_cancel(token);
    for (int i = 0; i < 3; i++) {
        await_call(&many[i], 5000);
        assert(many[i].code == CANCELLED);
    }
    async_demo_cancel_token_destroy(token);

    // Destroying the consumer's reference is not cancellation: the call
    // keeps its own reference and runs to its timeout.
    token = async_demo_cancel_token_create();
    call_reset(&c);
    async_demo_tasks_wait(100, token, on_i64, &c);
    async_demo_cancel_token_destroy(token);
    await_call(&c, 5000);
    assert(c.code == 0 && c.i64 == 100);
}

int main(void) {
    async_demo_error err = {0};

    assert(ASYNC_DEMO_ABI_VERSION == 3u);
    assert(async_demo_abi_version() == ASYNC_DEMO_ABI_VERSION);
    assert(async_demo_tasks_checksum() == ASYNC_DEMO_TASKS_CHECKSUM);

    run_task();
    run_batch();
    run_n_tasks();
    wait_and_cancel();

    // Every task body, cancelled ones included, has been dropped.
    for (int i = 0; i < 2000 && async_demo_tasks_active_callbacks(&err) != 0; i++) sleep_ms(1);
    assert(async_demo_tasks_active_callbacks(&err) == 0);
    assert(err.code == 0);

    ASSERT_NO_LEAKS(async_demo_debug_live);
    printf("c/async-demo: OK\n");
    return 0;
}
