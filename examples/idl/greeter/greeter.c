/* A producer written in C: it implements the header `weaveffi generate`
   wrote for greeter.yml (bindings/c/greeter.h). */
#define GREETER_BUILD /* export the symbols on Windows */
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include "greeter.h"

/* ANCHOR: api */
/* One contract function per declaration, as the header computed them. */
GREETER_DEFINE_CONTRACT

int32_t greeter_add(int32_t a, int32_t b, greeter_error* out_err) {
    memset(out_err, 0, sizeof *out_err); /* success: a cleared error */
    return (int32_t)((uint32_t)a + (uint32_t)b);
}

greeter_run greeter_greet(greeter_run name, greeter_error* out_err) {
    memset(out_err, 0, sizeof *out_err);
    static const char hello[] = "Hello, ";
    size_t len = sizeof hello - 1 + name.len + 1;
    uint8_t* text = greeter_alloc(len); /* the consumer frees it with greeter_run_free */
    if (text == NULL) {
        static const char oom[] = "out of memory";
        greeter_error_set(out_err, -1, (const uint8_t*)oom, sizeof oom - 1);
        greeter_run none = {NULL, 0};
        return none;
    }
    memcpy(text, hello, sizeof hello - 1);
    if (name.len != 0) memcpy(text + sizeof hello - 1, name.ptr, name.len);
    text[len - 1] = '!';
    greeter_run out = {text, len};
    return out;
}
/* ANCHOR_END: api */

/* ANCHOR: runtime */
/* The runtime surface every producer exports. */
uint32_t greeter_abi_version(void) { return GREETER_ABI_VERSION; }

/* One allocator for every run: returned strings and buffers, error messages
   and payloads, and runs a consumer allocates. calloc's alignment is at
   least 8 on every supported platform. */
uint8_t* greeter_alloc(size_t len) { return len != 0 ? calloc(len, 1) : NULL; }
void greeter_run_free(greeter_run run) { free((void*)run.ptr); }

static const uint8_t* copy(const uint8_t* ptr, size_t len) {
    uint8_t* run = greeter_alloc(len);
    if (run != NULL) memcpy(run, ptr, len);
    return run;
}

void greeter_error_clear(greeter_error* err) {
    if (err == NULL) return;
    free((void*)err->message_ptr);
    free((void*)err->payload_ptr);
    memset(err, 0, sizeof *err);
}

void greeter_error_free(greeter_error* err) {
    greeter_error_clear(err);
    free(err);
}

void greeter_error_set(greeter_error* err, int32_t code, const uint8_t* message_ptr,
                       size_t message_len) {
    if (err == NULL) return;
    greeter_error_clear(err);
    err->code = code;
    if (message_ptr != NULL && message_len != 0) {
        err->message_ptr = copy(message_ptr, message_len);
        err->message_len = err->message_ptr != NULL ? message_len : 0;
    }
}

void greeter_error_set_payload(greeter_error* err, const uint8_t* ptr, size_t len) {
    if (err == NULL) return;
    free((void*)err->payload_ptr);
    err->payload_ptr = ptr != NULL && len != 0 ? copy(ptr, len) : NULL;
    err->payload_len = err->payload_ptr != NULL ? len : 0;
}

/* Cancel tokens (this API has no async functions, but every library exports
   the same runtime surface). */
struct greeter_cancel_token {
    atomic_int refs;
    atomic_bool cancelled;
};

greeter_cancel_token* greeter_cancel_token_create(void) {
    greeter_cancel_token* token = malloc(sizeof *token);
    if (token != NULL) {
        atomic_init(&token->refs, 1);
        atomic_init(&token->cancelled, false);
    }
    return token;
}

void greeter_cancel_token_cancel(greeter_cancel_token* token) {
    if (token != NULL) atomic_store(&token->cancelled, true);
}

bool greeter_cancel_token_is_cancelled(const greeter_cancel_token* token) {
    return token != NULL && atomic_load(&((greeter_cancel_token*)token)->cancelled);
}

void greeter_cancel_token_destroy(greeter_cancel_token* token) {
    if (token != NULL && atomic_fetch_sub(&token->refs, 1) == 1) free(token);
}

/* Leak counters: kind -1 is 0 because this producer doesn't count. */
uint64_t greeter_debug_live(int32_t kind) {
    (void)kind;
    return 0;
}
/* ANCHOR_END: runtime */
