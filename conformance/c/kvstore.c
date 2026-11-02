// Conformance consumer: kvstore sample, C target (ABI revision 6).
//
// Drives the feature-complete producer through its generated header and
// `kvstore_buffer.h`:
//
//   * the load-time checks (the ABI revision and the contract), and the
//     per-declaration contract functions checked against FNV-1a hashes this
//     consumer computes from each path and canonical signature;
//   * the `Store` interface: fallible and infallible constructors, methods,
//     statics, the deprecated `size`, records (`Entry`, `StoreInfo`), the
//     C-style `EntryKind`, maps and optionals, and the logical clock;
//   * `KvError` codes with their payload fields decoded (`KeyNotFound`,
//     `Expired`, `StoreFull`, `Rejected`), plus marshalling failures (-3);
//   * lazy iterators of strings (throwing), records, objects, and optional
//     scalars, including a null `out_item` that loses no element;
//   * four callback interfaces implemented in C, each passed as a
//     `{ctx, vtable}` struct by value: a `Listener` (retained, filtered by
//     `accepts`, told about every `Change`, detached when it fails, and
//     notified from a producer thread during compaction), a `Policy` (an
//     optional-scalar parameter and return, a record return, a throwing
//     method whose typed error and payload reach the `put` caller, an object
//     parameter and object return, a null object and a malformed return
//     rejected), a `Loader` passed as an optional callback (string, bytes,
//     and optional-object returns; typed errors decoded by the producer or
//     passed through), and a `Scorer` (a typed array in and out);
//   * open vtables: a vtable shorter than the producer's, or with a null
//     method slot, is accepted, and calling the missing method fails (-4)
//     with a message naming it, which the sample reports as
//     `CallbackFailed`; a vtable shorter than its header is -3; a null
//     `free` is skipped;
//   * a thread-affine vtable whose value-returning method the producer
//     refuses to call off its thread;
//   * `Store` objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * async calls completing with `(context, err, result)`: an async free
//     function returning an object, a cancellable method cancelled
//     mid-pause (completing with -5 while its background work stops
//     cooperatively, shown by `active_jobs`), an async list, an optional
//     scalar, a typed array, and concurrent launches;
//   * declarations from the sample's other Rust modules (`summarize`,
//     `summarize_all`, `render_report`, `ReportError`), which share the one
//     namespace;
//   * a `usize` return, a `throws: any` method, and a callback failure
//     converted into the domain's `CallbackFailed`.
//
// Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>

#include "kvstore_buffer.h"

HARNESS_RUNS(kvstore)

static pthread_t g_main_thread;

static const kvstore_run EMPTY_RUN = {NULL, 0};
static const kvstore_opt_i64 NO_TTL = {false, 0};

// ── small helpers ──────────────────────────────────────────────────────────

static int str_is(kvstore_str s, const char* expected) {
    return bytes_eq((const uint8_t*)s.ptr, s.len, expected);
}

static int starts_with(kvstore_str s, const char* prefix) {
    size_t n = strlen(prefix);
    return s.len >= n && memcmp(s.ptr, prefix, n) == 0;
}

static kvstore_str run_str(kvstore_run r) {
    kvstore_str s = {(const char*)r.ptr, r.len};
    return s;
}

// An owned, NUL-terminated copy of a returned string, which is released.
static char* take_string(kvstore_run run) {
    char* s = (char*)calloc(run.len + 1, 1);
    if (run.len > 0) memcpy(s, run.ptr, run.len);
    kvstore_run_free(run);
    return s;
}

// Copy a writer's bytes into a run the producer adopts.
static kvstore_run hand_over(kvstore_writer* w) {
    assert(!w->failed);
    kvstore_run run = {kvstore_alloc(w->len), w->len};
    if (w->len > 0) memcpy((uint8_t*)run.ptr, w->ptr, w->len);
    kvstore_writer_free(w);
    return run;
}

static kvstore_run hand_over_text(const char* s) {
    size_t n = strlen(s);
    kvstore_run run = {kvstore_alloc(n), n};
    if (n > 0) memcpy((uint8_t*)run.ptr, s, n);
    return run;
}

static kvstore_opt_i64 ttl_of(const int64_t* ttl) {
    kvstore_opt_i64 o = {ttl != NULL, ttl != NULL ? *ttl : 0};
    return o;
}

static kvstore_Store* open_store(const char* path) {
    kvstore_error err;
    kvstore_Store* s = kvstore_Store_open(str_run(path), &err);
    assert(err.code == 0 && s != NULL);
    return s;
}

static uint64_t count(const kvstore_Store* s) {
    kvstore_error err;
    uint64_t n = kvstore_Store_count(s, &err);
    assert(err.code == 0);
    return n;
}

static char* path_of(const kvstore_Store* s) {
    kvstore_error err;
    kvstore_run p = kvstore_Store_path(s, &err);
    assert(err.code == 0);
    return take_string(p);
}

static int path_is(const kvstore_Store* s, const char* expected) {
    char* p = path_of(s);
    int ok = strcmp(p, expected) == 0;
    free(p);
    return ok;
}

// put(key, value, kind, ttl?): decodes the returned entry into `out` on
// success; on failure leaves the error in `err`.
static bool try_put(const kvstore_Store* s, const char* key, const char* value,
                    kvstore_EntryKind kind, const int64_t* ttl, kvstore_Entry* out,
                    kvstore_error* err) {
    kvstore_run buf = kvstore_Store_put(s, str_run(key), str_run(value), kind, ttl_of(ttl), err);
    if (err->code != 0) {
        assert(buf.ptr == NULL && buf.len == 0);
        return false;
    }
    kvstore_Entry e;
    assert(kvstore_Entry_take(buf, &e));
    if (out != NULL) {
        *out = e;
    } else {
        kvstore_Entry_free(&e);
    }
    return true;
}

static void put(const kvstore_Store* s, const char* key, const char* value) {
    kvstore_error err;
    assert(try_put(s, key, value, kvstore_EntryKind_Persistent, NULL, NULL, &err));
}

// The optional-string parameter buffer for a prefix.
static kvstore_writer prefix_buf(const char* prefix) {
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_str view = kvstore_str_of(prefix);
    kvstore_buf_opt_string_write(&w, prefix ? &view : NULL);
    return w;
}

static void expect_key_not_found(kvstore_error* err, const char* key) {
    assert(err->code == kvstore_KvError_KeyNotFound);
    kvstore_KvError_KeyNotFound_payload p;
    assert(kvstore_KvError_KeyNotFound_payload_decode(err->payload_ptr, err->payload_len, &p));
    assert(str_is(p.key, key));
    kvstore_KvError_KeyNotFound_payload_free(&p);
    kvstore_error_clear(err);
}

// `CallbackFailed` (a consumer callback's failure, converted by the sample),
// with the consumer's message when `message` isn't NULL.
static void expect_callback_failed(kvstore_error* err, const char* message) {
    assert(err->code == kvstore_KvError_CallbackFailed);
    kvstore_KvError_CallbackFailed_payload p;
    assert(kvstore_KvError_CallbackFailed_payload_decode(err->payload_ptr, err->payload_len, &p));
    assert(p.message.len > 0);
    if (message != NULL) {
        if (!MSG_EQ(*err, message)) {
            fprintf(stderr, "expected callback failure \"%s\", got \"%.*s\"\n", message,
                    (int)err->message_len, (const char*)err->message_ptr);
        }
        assert(MSG_EQ(*err, message) && str_is(p.message, message));
    }
    kvstore_KvError_CallbackFailed_payload_free(&p);
    kvstore_error_clear(err);
}

// The failure the producer reports for a method a consumer's vtable lacks.
static void expect_missing_method(kvstore_error* err, const char* method) {
    char message[128];
    snprintf(message, sizeof message,
             "the binding doesn't implement callback method `%s`; regenerate it", method);
    expect_callback_failed(err, message);
}

// ── listener (consumer-implemented, retained) ──────────────────────────────

typedef struct {
    const char* skip;     // accepts() answers false for this key
    const char* fail_on;  // accepts() reports a failure for this key
    atomic_int accepted, puts, removed, expired, cleared;
    atomic_int off_main_thread;  // on_change calls on a producer thread
    // The last Put's entry fields and the last Removed key.
    uint32_t last_version;
    bool last_replaced;
    char last_key[64];
    uint32_t last_cleared;
} listener_ctx;

static atomic_int g_listeners_freed;
// The `accepts` and `on_change` calls of the last listener released.
static atomic_int g_freed_accepted, g_freed_puts;
// Every listener's calls made off the main thread, accepts and on_change.
static atomic_int g_listener_calls_off_main;

static bool listener_accepts(void* ctx, kvstore_run key, kvstore_error* out_err) {
    listener_ctx* l = (listener_ctx*)ctx;
    if (!pthread_equal(pthread_self(), g_main_thread)) atomic_fetch_add(&g_listener_calls_off_main, 1);
    atomic_fetch_add(&l->accepted, 1);
    if (l->fail_on != NULL && run_eq(key, l->fail_on)) {
        kvstore_error_set(out_err, -1, (const uint8_t*)"listener refused", 16);
        return false;
    }
    return l->skip == NULL || !run_eq(key, l->skip);
}

// The Change buffer is borrowed for the call.
static void listener_on_change(void* ctx, kvstore_run change, kvstore_error* out_err) {
    (void)out_err;
    listener_ctx* l = (listener_ctx*)ctx;
    if (!pthread_equal(pthread_self(), g_main_thread)) {
        atomic_fetch_add(&l->off_main_thread, 1);
        atomic_fetch_add(&g_listener_calls_off_main, 1);
    }
    kvstore_Change c;
    assert(kvstore_Change_decode(change.ptr, change.len, &c));
    switch (c.tag) {
    case kvstore_Change_Put:
        l->last_version = c.as.Put.entry.version;
        l->last_replaced = c.as.Put.replaced;
        snprintf(l->last_key, sizeof l->last_key, "%s", c.as.Put.entry.key.ptr);
        atomic_fetch_add(&l->puts, 1);
        break;
    case kvstore_Change_Removed:
        snprintf(l->last_key, sizeof l->last_key, "%s", c.as.Removed.key.ptr);
        atomic_fetch_add(c.as.Removed.expired ? &l->expired : &l->removed, 1);
        break;
    case kvstore_Change_Cleared:
        l->last_cleared = c.as.Cleared.count;
        atomic_fetch_add(&l->cleared, 1);
        break;
    }
    kvstore_Change_free(&c);
}

static void listener_free(void* ctx) {
    listener_ctx* l = (listener_ctx*)ctx;
    atomic_store(&g_freed_accepted, atomic_load(&l->accepted));
    atomic_store(&g_freed_puts, atomic_load(&l->puts));
    free(ctx);
    atomic_fetch_add(&g_listeners_freed, 1);
}

static const kvstore_Listener_vtable LISTENER_VTABLE = {
    sizeof(kvstore_Listener_vtable), 0, listener_free, listener_accepts, listener_on_change,
};

// The same listener, declared thread-affine: the producer calls `accepts`
// (which returns a value) only on the thread that subscribed it, and fails
// such a call from any other thread with -4 instead of making it.
static const kvstore_Listener_vtable AFFINE_LISTENER_VTABLE = {
    sizeof(kvstore_Listener_vtable), KVSTORE_VTABLE_THREAD_AFFINE, listener_free,
    listener_accepts, listener_on_change,
};

// A vtable compiled before `on_change` was appended: its `size` ends at the
// `on_change` slot (the entry is still set, to show the producer never
// reads past `size`).
static const kvstore_Listener_vtable SHORT_LISTENER_VTABLE = {
    (uint32_t)offsetof(kvstore_Listener_vtable, on_change), 0, listener_free, listener_accepts,
    listener_on_change,
};

static listener_ctx* new_listener(const char* skip, const char* fail_on) {
    listener_ctx* l = (listener_ctx*)calloc(1, sizeof *l);
    assert(l != NULL);
    l->skip = skip;
    l->fail_on = fail_on;
    return l;
}

static kvstore_Listener listener_arg(listener_ctx* l, const kvstore_Listener_vtable* vtable) {
    kvstore_Listener arg = {l, vtable};
    return arg;
}

// ── policy (consumer-implemented, rich returns, throws) ────────────────────

typedef struct {
    kvstore_Store* other;  // owned reference: where "b/" keys go
    atomic_int admitted;
} policy_ctx;

static atomic_int g_policies_freed;

// ttl_for: an optional scalar parameter and return. "short" lives one tick,
// "forever" never expires, "ttl-fail" fails with a typed error, and every
// other key keeps the requested TTL.
static kvstore_opt_i64 policy_ttl_for(void* ctx, kvstore_run key, kvstore_opt_i64 requested,
                                      kvstore_error* out_err) {
    (void)ctx;
    assert(requested.has || requested.value == 0);
    const kvstore_opt_i64 one = {true, 1};
    if (run_eq(key, "short")) return one;
    if (run_eq(key, "forever")) return NO_TTL;
    if (run_eq(key, "ttl-fail")) {
        kvstore_error_set(out_err, kvstore_KvError_InvalidPath, (const uint8_t*)"no ttl for you", 14);
        return NO_TTL;
    }
    return requested;
}

static kvstore_run policy_admit(void* ctx, kvstore_run entry, kvstore_error* out_err) {
    policy_ctx* p = (policy_ctx*)ctx;
    atomic_fetch_add(&p->admitted, 1);
    kvstore_Entry e;
    assert(kvstore_Entry_decode(entry.ptr, entry.len, &e));
    assert(e.version == 0 && "the store assigns the version after admission");
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_run out = EMPTY_RUN;
    if (starts_with(e.key, "secret")) {
        // A typed domain error with its fields as the payload.
        kvstore_error_set(out_err, kvstore_KvError_Rejected,
                          (const uint8_t*)"secrets are not stored", 22);
        kvstore_KvError_Rejected_payload r;
        r.key = e.key;
        r.reason = kvstore_str_of("no secrets");
        kvstore_KvError_Rejected_payload_write(&w, &r);
        kvstore_error_set_payload(out_err, w.ptr, w.len);
        kvstore_writer_free(&w);
    } else if (starts_with(e.key, "boom")) {
        kvstore_error_set(out_err, 4242, (const uint8_t*)"policy exploded", 15);  // not a domain code
    } else if (starts_with(e.key, "garbage")) {
        out = hand_over_text("?");  // not an Entry: -3
    } else {
        // Rewrite: tag it and store it encrypted (and try to rename it,
        // which the store ignores).
        kvstore_buf_list_string original_tags = e.tags;
        kvstore_str original_key = e.key;
        kvstore_str tags[1] = {kvstore_str_of("admitted")};
        e.tags.items = tags;
        e.tags.len = 1;
        e.kind = kvstore_EntryKind_Encrypted;
        e.key = kvstore_str_of("renamed");
        kvstore_Entry_write(&w, &e);
        e.tags = original_tags;
        e.key = original_key;
        out = hand_over(&w);
    }
    kvstore_Entry_free(&e);
    return out;
}

// `home` arrives as one strong reference this policy owns; the return is one
// strong reference the producer adopts.
static kvstore_Store* policy_route(void* ctx, kvstore_run key, kvstore_Store* home,
                                   kvstore_error* out_err) {
    (void)out_err;
    policy_ctx* p = (policy_ctx*)ctx;
    if (starts_with(run_str(key), "b/")) {
        kvstore_Store_destroy(home);
        return kvstore_Store_clone(p->other);
    }
    if (starts_with(run_str(key), "null/")) {
        kvstore_Store_destroy(home);
        return NULL;  // a required object may not be null: -3
    }
    return home;
}

static void policy_free(void* ctx) {
    policy_ctx* p = (policy_ctx*)ctx;
    kvstore_Store_destroy(p->other);
    free(p);
    atomic_fetch_add(&g_policies_freed, 1);
}

static const kvstore_Policy_vtable POLICY_VTABLE = {
    sizeof(kvstore_Policy_vtable), 0, policy_free, policy_ttl_for, policy_admit, policy_route,
};

// A policy vtable that ends before `route` (whose slot is still set).
static const kvstore_Policy_vtable SHORT_POLICY_VTABLE = {
    (uint32_t)offsetof(kvstore_Policy_vtable, route), 0, policy_free, policy_ttl_for,
    policy_admit, policy_route,
};

static policy_ctx* new_policy(kvstore_Store* other) {
    policy_ctx* p = (policy_ctx*)calloc(1, sizeof *p);
    assert(p != NULL);
    p->other = kvstore_Store_clone(other);
    return p;
}

static kvstore_Policy policy_arg(policy_ctx* p, const kvstore_Policy_vtable* vtable) {
    kvstore_Policy arg = {p, vtable};
    return arg;
}

// ── loader (consumer-implemented, passed as an optional parameter) ─────────

typedef struct {
    kvstore_Store* fallback;  // owned reference or NULL
} loader_ctx;

static atomic_int g_loaders_freed;

static kvstore_run loader_name(void* ctx, kvstore_error* out_err) {
    (void)ctx;
    (void)out_err;
    return hand_over_text("c-loader");
}

static kvstore_Store* loader_fallback(void* ctx, kvstore_run key, kvstore_error* out_err) {
    (void)out_err;
    loader_ctx* l = (loader_ctx*)ctx;
    if (l->fallback != NULL && run_eq(key, "fb")) {
        return kvstore_Store_clone(l->fallback);
    }
    return NULL;  // `Store?`: none
}

static void fail_not_found(kvstore_error* out_err, const char* key) {
    kvstore_error_set(out_err, kvstore_KvError_KeyNotFound, (const uint8_t*)"not in the loader", 17);
    kvstore_KvError_KeyNotFound_payload p;
    p.key = kvstore_str_of(key);
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_KvError_KeyNotFound_payload_write(&w, &p);
    kvstore_error_set_payload(out_err, w.ptr, w.len);
    kvstore_writer_free(&w);
}

static kvstore_run loader_load(void* ctx, kvstore_run key, kvstore_error* out_err) {
    (void)ctx;
    if (run_eq(key, "missing")) {
        fail_not_found(out_err, "missing");  // this key: get_or_load returns none
        return EMPTY_RUN;
    }
    if (run_eq(key, "elsewhere")) {
        fail_not_found(out_err, "other");  // another key: passed through
        return EMPTY_RUN;
    }
    if (run_eq(key, "broken")) {
        kvstore_error_set(out_err, -1, (const uint8_t*)"loader is broken", 16);
        return EMPTY_RUN;
    }
    char value[80];
    snprintf(value, sizeof value, "loaded:%.*s", (int)key.len, (const char*)key.ptr);
    return hand_over_text(value);
}

static void loader_free(void* ctx) {
    loader_ctx* l = (loader_ctx*)ctx;
    kvstore_Store_destroy(l->fallback);
    free(l);
    atomic_fetch_add(&g_loaders_freed, 1);
}

static const kvstore_Loader_vtable LOADER_VTABLE = {
    sizeof(kvstore_Loader_vtable), 0, loader_free, loader_name, loader_fallback, loader_load,
};

// A full-size loader vtable whose `load` slot is null.
static const kvstore_Loader_vtable NO_LOAD_VTABLE = {
    sizeof(kvstore_Loader_vtable), 0, loader_free, loader_name, loader_fallback, NULL,
};

static loader_ctx* new_loader(kvstore_Store* fallback) {
    loader_ctx* l = (loader_ctx*)calloc(1, sizeof *l);
    assert(l != NULL);
    l->fallback = fallback;
    return l;
}

// get_or_load through a fresh loader (or none) with `vtable`; decodes
// `Entry?` into `out` (NULL when absent) and returns false with `err` set on
// failure.
static bool get_or_load_with(const kvstore_Store* s, const char* key, loader_ctx* loader,
                             const kvstore_Loader_vtable* vtable, kvstore_Entry** out,
                             kvstore_error* err) {
    kvstore_Loader arg = {loader, loader ? vtable : NULL};
    kvstore_run buf = kvstore_Store_get_or_load(s, str_run(key), arg, err);
    if (err->code != 0) {
        assert(buf.ptr == NULL);
        return false;
    }
    assert(kvstore_buf_opt_Entry_take(buf, out));
    return true;
}

static bool get_or_load(const kvstore_Store* s, const char* key, loader_ctx* loader,
                        kvstore_Entry** out, kvstore_error* err) {
    return get_or_load_with(s, key, loader, &LOADER_VTABLE, out, err);
}

// ── scorer (consumer-implemented, typed arrays in and out) ─────────────────

typedef enum { SCORE_SIZES, SCORE_FAIL, SCORE_SHORT } score_mode;

typedef struct {
    score_mode mode;
} scorer_ctx;

static atomic_int g_scorers_freed;

// `sizes` is a borrowed typed array (`len` in bytes); the scores are a run
// of `count * sizeof(double)` bytes from kvstore_alloc, which the producer
// adopts.
static kvstore_run scorer_scores(void* ctx, kvstore_run sizes, kvstore_error* out_err) {
    scorer_ctx* sc = (scorer_ctx*)ctx;
    assert(((uintptr_t)sizes.ptr % 8) == 0 && "a borrowed typed array is aligned");
    assert(sizes.len % sizeof(uint64_t) == 0);
    const uint64_t* size_of = (const uint64_t*)(const void*)sizes.ptr;
    size_t entries = sizes.len / sizeof(uint64_t);
    if (sc->mode == SCORE_SIZES) {
        // The store's value sizes in key order: a="1", b="333", c="22".
        assert(entries == 3 && size_of[0] == 1 && size_of[1] == 3 && size_of[2] == 2);
    }
    if (sc->mode == SCORE_FAIL) {
        kvstore_error_set(out_err, -1, (const uint8_t*)"scorer is out of order", 22);
        return EMPTY_RUN;
    }
    size_t n = sc->mode == SCORE_SHORT ? 1 : entries;
    kvstore_run out = {kvstore_alloc(n * sizeof(double)), n * sizeof(double)};
    double* scores = (double*)(void*)(uint8_t*)out.ptr;
    for (size_t i = 0; i < n; i++) scores[i] = (double)size_of[i];
    return out;
}

static void scorer_free(void* ctx) {
    free(ctx);
    atomic_fetch_add(&g_scorers_freed, 1);
}

static const kvstore_Scorer_vtable SCORER_VTABLE = {
    sizeof(kvstore_Scorer_vtable), 0, scorer_free, scorer_scores,
};

// Just the header: a binding generated before `scores` existed.
static const kvstore_Scorer_vtable HEADER_ONLY_SCORER_VTABLE = {
    (uint32_t)offsetof(kvstore_Scorer_vtable, scores), 0, scorer_free, scorer_scores,
};

static kvstore_Scorer new_scorer(score_mode mode, const kvstore_Scorer_vtable* vtable) {
    scorer_ctx* sc = (scorer_ctx*)calloc(1, sizeof *sc);
    assert(sc != NULL);
    sc->mode = mode;
    kvstore_Scorer arg = {sc, vtable};
    return arg;
}

// ── async completion state ─────────────────────────────────────────────────

typedef struct {
    atomic_int done;
    int32_t code;
    char message[128];
    uint32_t u32;
    kvstore_Store* store;
    uint8_t* buf;
    size_t len;
} call_state;

static void settle(call_state* c, kvstore_error* err) {
    c->code = err ? err->code : 0;
    if (err != NULL && err->message_ptr != NULL) {
        snprintf(c->message, sizeof c->message, "%.*s", (int)err->message_len,
                 (const char*)err->message_ptr);
    }
    kvstore_error_free(err);
}

static void on_u32(void* context, kvstore_error* err, uint32_t result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    c->u32 = result;
    atomic_store(&c->done, 1);
}

static void on_store(void* context, kvstore_error* err, kvstore_Store* result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    c->store = result;
    atomic_store(&c->done, 1);
}

// A run async result (a buffer or a typed array) is owned: copy it, then
// release it.
static void on_run(void* context, kvstore_error* err, kvstore_run result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    if (result.ptr != NULL) {
        c->buf = (uint8_t*)malloc(result.len);
        memcpy(c->buf, result.ptr, result.len);
        c->len = result.len;
    }
    kvstore_run_free(result);
    atomic_store(&c->done, 1);
}

static void on_opt_u32(void* context, kvstore_error* err, kvstore_opt_u32 result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    assert(result.has || result.value == 0);
    c->u32 = result.has ? result.value : UINT32_MAX;
    atomic_store(&c->done, 1);
}

static void wait_for(call_state* c) {
    for (int i = 0; i < 5000 && !atomic_load(&c->done); i++) sleep_ms(1);
    assert(atomic_load(&c->done) && "async call completed");
}

// ── sections ───────────────────────────────────────────────────────────────

// Every declaration exports `kvstore_contract_{fnv1a(path)}`, returning
// fnv1a of its canonical signature. Recompute a sample of each kind here,
// independently of the generated table `kvstore_contract_check` walks.
static void contracts(void) {
    assert(kvstore_contract_check() == NULL);

    // Interface members: a constructor, methods, a static, an iterator, a
    // `usize` (`u64`), `throws any`, and async ones (the cancel token isn't
    // a parameter).
    assert(fnv1a64("Store") == 0x04ae3c13e8c60feeull);
    assert(kvstore_contract_04ae3c13e8c60fee() == fnv1a64("interface Store"));
    assert(fnv1a64("Store.open") == 0x7b4181d5fd2958e8ull);
    assert(kvstore_contract_7b4181d5fd2958e8() ==
           fnv1a64("constructor open(string) -> Store throws KvError"));
    assert(fnv1a64("Store.get") == 0xdc5ad183936cb6fcull);
    assert(kvstore_contract_dc5ad183936cb6fc() ==
           fnv1a64("method get(string) -> Entry throws KvError"));
    assert(fnv1a64("Store.put") == 0x2bef5883c0f5eed1ull);
    assert(kvstore_contract_2bef5883c0f5eed1() ==
           fnv1a64("method put(string, bytes, EntryKind, i64?) -> Entry throws KvError"));
    assert(fnv1a64("Store.default_capacity") == 0xeb1707cd00005b2aull);
    assert(kvstore_contract_eb1707cd00005b2a() == fnv1a64("static default_capacity() -> u32"));
    assert(fnv1a64("Store.keys") == 0xf6f7b1b7777fe504ull);
    assert(kvstore_contract_f6f7b1b7777fe504() ==
           fnv1a64("method keys(string?) -> iter<string> throws KvError"));
    assert(fnv1a64("Store.import_lines") == 0xf4eefcd48c5343adull);
    assert(kvstore_contract_f4eefcd48c5343ad() ==
           fnv1a64("method import_lines(string) -> u64 throws any"));
    assert(fnv1a64("Store.compact") == 0x517e0558a7be8245ull);
    assert(kvstore_contract_517e0558a7be8245() ==
           fnv1a64("method compact(u32) -> u32 async cancellable"));
    assert(fnv1a64("Store.subscribe") == 0xd5d5dde9b513faa8ull);
    assert(kvstore_contract_d5d5dde9b513faa8() == fnv1a64("method subscribe(Listener) -> u32"));
    // Free functions, including ones from the sample's other Rust modules.
    assert(fnv1a64("open_store") == 0x560685d2b0e40443ull);
    assert(kvstore_contract_560685d2b0e40443() ==
           fnv1a64("function open_store(string) -> Store throws KvError async"));
    assert(fnv1a64("summarize") == 0x20ea7db5ad4182daull);
    assert(kvstore_contract_20ea7db5ad4182da() ==
           fnv1a64("function summarize(Store, string?) -> Stats throws KvError"));
    assert(fnv1a64("render_report") == 0x64c6e78d578f5474ull);
    assert(kvstore_contract_64c6e78d578f5474() ==
           fnv1a64("function render_report([Entry]) -> [string] throws ReportError"));
    // Callback interfaces and their methods, with their vtable slots.
    assert(fnv1a64("Listener") == 0x207a79333043f971ull);
    assert(kvstore_contract_207a79333043f971() == fnv1a64("callback Listener"));
    assert(fnv1a64("Listener.on_change") == 0x3dfdc5f75d508f7dull);
    assert(kvstore_contract_3dfdc5f75d508f7d() ==
           fnv1a64("callback_method #1 on_change(Change) -> void throws any"));
    assert(fnv1a64("Policy.ttl_for") == 0x5e446dcc80cf98adull);
    assert(kvstore_contract_5e446dcc80cf98ad() ==
           fnv1a64("callback_method #0 ttl_for(string, i64?) -> i64? throws KvError"));
    assert(fnv1a64("Policy.route") == 0xf7c9d705ed5b75b0ull);
    assert(kvstore_contract_f7c9d705ed5b75b0() ==
           fnv1a64("callback_method #2 route(string, Store) -> Store throws any"));
    // Records, enums, error domains, and codes.
    assert(fnv1a64("Entry") == 0x7dea6d25be21fb73ull);
    assert(kvstore_contract_7dea6d25be21fb73() ==
           fnv1a64("record Entry {string, bytes, EntryKind, u32, i64?, [string], {string:string}}"));
    assert(fnv1a64("Stats") == 0xae238413b7c6ebacull);
    assert(kvstore_contract_ae238413b7c6ebac() == fnv1a64("record Stats {u32, u64, {EntryKind:u32}}"));
    assert(fnv1a64("EntryKind") == 0x7a91662ff203bc79ull);
    assert(kvstore_contract_7a91662ff203bc79() ==
           fnv1a64("enum EntryKind {Volatile = 0, Persistent = 1, Encrypted = 2}"));
    assert(fnv1a64("Change") == 0x2f64461723d3df5dull);
    assert(kvstore_contract_2f64461723d3df5d() ==
           fnv1a64("enum Change {Put = 0 {Entry, bool}, Removed = 1 {string, bool}, Cleared = 2 {u32}}"));
    assert(fnv1a64("KvError") == 0xd9f479a1c7fedaa2ull);
    assert(kvstore_contract_d9f479a1c7fedaa2() == fnv1a64("errors KvError"));
    assert(fnv1a64("KvError.KeyNotFound") == 0x6a57dd1417ff9598ull);
    assert(kvstore_contract_6a57dd1417ff9598() == fnv1a64("code KeyNotFound = 1001 {string}"));
    assert(fnv1a64("KvError.InvalidPath") == 0x51a61073ad3448d0ull);
    assert(kvstore_contract_51a61073ad3448d0() == fnv1a64("code InvalidPath = 1004"));
    assert(fnv1a64("ReportError.NothingToReport") == 0x50a582faf5acc5afull);
    assert(kvstore_contract_50a582faf5acc5af() == fnv1a64("code NothingToReport = 2001"));
}

static void constructors(void) {
    kvstore_error err;

    // Fallible constructor: an empty path is InvalidPath (no payload).
    assert(kvstore_Store_open(EMPTY_RUN, &err) == NULL);
    assert(err.code == kvstore_KvError_InvalidPath);
    assert(MSG_EQ(err, "invalid path"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    kvstore_error_clear(&err);
    // A null run with a nonzero length is a marshalling failure.
    const kvstore_run null_run = {NULL, 4};
    assert(kvstore_Store_open(null_run, &err) == NULL && err.code == -3);
    kvstore_error_clear(&err);

    kvstore_Store* s = kvstore_Store_new(&err);
    assert(err.code == 0 && s != NULL && path_is(s, "memory"));
    assert(kvstore_Store_capacity(s, &err) == kvstore_Store_default_capacity(&err));
    assert(kvstore_Store_default_capacity(&err) == 1000000);
    kvstore_Store_destroy(s);

    // The async free function completes with a new object...
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_open_store(str_run("/async"), on_store, &c);
    wait_for(&c);
    assert(c.code == 0 && c.store != NULL && path_is(c.store, "/async"));
    kvstore_Store_destroy(c.store);
    // ...or with the typed error (and a null result).
    memset(&c, 0, sizeof c);
    kvstore_open_store(EMPTY_RUN, on_store, &c);
    wait_for(&c);
    assert(c.code == kvstore_KvError_InvalidPath && c.store == NULL);
    assert(strcmp(c.message, "invalid path") == 0);
    // An input marshalling failure is delivered through the completion too.
    memset(&c, 0, sizeof c);
    kvstore_open_store(null_run, on_store, &c);
    wait_for(&c);
    assert(c.code == -3 && c.store == NULL);
}

static void basics(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/basics");

    // put returns the stored entry; the version counts puts of the key.
    kvstore_Entry e;
    assert(try_put(s, "alpha", "one", kvstore_EntryKind_Persistent, NULL, &e, &err));
    assert(str_is(e.key, "alpha") && e.value.len == 3 && memcmp(e.value.ptr, "one", 3) == 0);
    assert(e.kind == kvstore_EntryKind_Persistent && e.version == 1);
    assert(e.expires_at == NULL && e.tags.len == 0 && e.metadata.len == 0);
    kvstore_Entry_free(&e);
    assert(try_put(s, "alpha", "two", kvstore_EntryKind_Volatile, NULL, &e, &err));
    assert(e.version == 2 && e.kind == kvstore_EntryKind_Volatile);
    kvstore_Entry_free(&e);

    // get (throwing record) and find (optional record).
    kvstore_run buf = kvstore_Store_get(s, str_run("alpha"), &err);
    assert(err.code == 0 && buf.ptr != NULL);
    assert(kvstore_Entry_take(buf, &e));
    assert(e.value.len == 3 && memcmp(e.value.ptr, "two", 3) == 0);
    kvstore_Entry_free(&e);
    buf = kvstore_Store_get(s, str_run("nope"), &err);
    assert(buf.ptr == NULL && buf.len == 0);
    assert(MSG_EQ(err, "key not found: nope"));
    expect_key_not_found(&err, "nope");

    kvstore_Entry* found = NULL;
    assert(kvstore_buf_opt_Entry_take(kvstore_Store_find(s, str_run("alpha"), &err), &found));
    assert(err.code == 0 && found != NULL && found->version == 2);
    kvstore_buf_opt_Entry_free(&found);
    assert(kvstore_buf_opt_Entry_take(kvstore_Store_find(s, str_run("nope"), &err), &found));
    assert(err.code == 0 && found == NULL);

    // TTLs follow the logical clock; an expired get reports when.
    const int64_t ttl = 10;
    assert(try_put(s, "ttl", "x", kvstore_EntryKind_Volatile, &ttl, &e, &err));
    assert(e.expires_at != NULL && *e.expires_at == 10);
    kvstore_Entry_free(&e);
    assert(kvstore_Store_now(s, &err) == 0);
    assert(kvstore_Store_tick(s, 9, &err) == 9 && count(s) == 2);
    assert(kvstore_Store_tick(s, 1, &err) == 10 && count(s) == 1);
    assert(kvstore_Store_get(s, str_run("ttl"), &err).ptr == NULL);
    assert(err.code == kvstore_KvError_Expired);
    kvstore_KvError_Expired_payload expired;
    assert(kvstore_KvError_Expired_payload_decode(err.payload_ptr, err.payload_len, &expired));
    assert(str_is(expired.key, "ttl") && expired.expired_at == 10);
    kvstore_KvError_Expired_payload_free(&expired);
    kvstore_error_clear(&err);
    assert(kvstore_Store_get(s, str_run("ttl"), &err).ptr == NULL);
    expect_key_not_found(&err, "ttl");  // the expired read removed it

    // Capacity: a new key past it is StoreFull { capacity }.
    kvstore_Store_set_capacity(s, 1, &err);
    assert(err.code == 0 && kvstore_Store_capacity(s, &err) == 1);
    put(s, "alpha", "three");  // replacing is fine
    assert(!try_put(s, "beta", "b", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_KvError_StoreFull);
    kvstore_KvError_StoreFull_payload full;
    assert(kvstore_KvError_StoreFull_payload_decode(err.payload_ptr, err.payload_len, &full));
    assert(full.capacity == 1);
    kvstore_error_clear(&err);
    kvstore_Store_set_capacity(s, 100, &err);

    // An undeclared enum value and invalid UTF-8 are marshalling failures.
    assert(!try_put(s, "k", "v", (kvstore_EntryKind)9, NULL, NULL, &err));
    assert(err.code == -3);
    kvstore_error_clear(&err);
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    buf = kvstore_Store_put(s, bytes_run(bad_utf8, 2), bytes_run(bad_utf8, 2),
                            kvstore_EntryKind_Volatile, NO_TTL, &err);
    assert(buf.ptr == NULL && err.code == -3);
    kvstore_error_clear(&err);
    // A method on a null receiver too.
    kvstore_Store_count(NULL, &err);
    assert(err.code == -3);
    kvstore_error_clear(&err);

    // delete, clear, and the deprecated size().
    put(s, "beta", "b");
    assert(kvstore_Store_delete(s, str_run("beta"), &err) && err.code == 0);
    assert(!kvstore_Store_delete(s, str_run("beta"), &err));
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    assert(kvstore_Store_size(s, &err) == 1);
#pragma clang diagnostic pop
    assert(kvstore_Store_clear(s, &err) == 1 && count(s) == 0);
    kvstore_Store_destroy(s);
}

static void iterators(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/iter");
    put(s, "user.bob", "b");
    put(s, "user.alice", "a");
    put(s, "sys.x", "xx");

    // keys(prefix?): owned string runs in key order.
    const char* want_all[3] = {"sys.x", "user.alice", "user.bob"};
    kvstore_writer w = prefix_buf(NULL);
    kvstore_Store_KeysIterator* it = kvstore_Store_keys(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    assert(err.code == 0 && it != NULL);
    kvstore_run item;
    int n = 0;
    while (kvstore_Store_KeysIterator_next(it, &item, &err) == 1) {
        assert(n < 3 && run_take_eq(item, want_all[n]));
        n++;
    }
    assert(n == 3 && err.code == 0);
    // Pulling past the end stays at the end.
    assert(kvstore_Store_KeysIterator_next(it, &item, &err) == 0 && err.code == 0);
    kvstore_Store_KeysIterator_destroy(it);

    // A prefix that matches nothing is KeyNotFound { key: prefix }.
    w = prefix_buf("zzz");
    assert(kvstore_Store_keys(s, kvstore_writer_run(&w), &err) == NULL);
    kvstore_writer_free(&w);
    expect_key_not_found(&err, "zzz");

    // Abandoning an iterator part-way releases it (and its items so far).
    w = prefix_buf("user.");
    it = kvstore_Store_keys(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    assert(kvstore_Store_KeysIterator_next(it, &item, &err) == 1);
    assert(run_take_eq(item, "user.alice"));
    kvstore_Store_KeysIterator_destroy(it);

    // entries(prefix?): records.
    w = prefix_buf("sys.");
    kvstore_Store_EntriesIterator* et = kvstore_Store_entries(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    assert(kvstore_Store_EntriesIterator_next(et, &item, &err) == 1);
    kvstore_Entry e;
    assert(kvstore_Entry_take(item, &e));
    assert(str_is(e.key, "sys.x") && e.value.len == 2);
    kvstore_Entry_free(&e);
    assert(kvstore_Store_EntriesIterator_next(et, &item, &err) == 0);
    kvstore_Store_EntriesIterator_destroy(et);

    // partition(prefixes): objects, created as they're pulled.
    kvstore_str prefixes[3] = {kvstore_str_of("user."), kvstore_str_of("sys."),
                               kvstore_str_of("none.")};
    kvstore_buf_list_string pl;
    pl.items = prefixes;
    pl.len = 3;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_string_write(&w, &pl);
    kvstore_Store_PartitionIterator* pt = kvstore_Store_partition(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    // A null `out_item` fails with -3 before an element is pulled, so the
    // first partition isn't lost.
    assert(kvstore_Store_PartitionIterator_next(pt, NULL, &err) == 0 && err.code == -3);
    kvstore_error_clear(&err);
    const uint32_t want_counts[3] = {2, 1, 0};
    kvstore_Store* part = NULL;
    n = 0;
    while (kvstore_Store_PartitionIterator_next(pt, &part, &err) == 1) {
        assert(part != NULL && part != s && count(part) == want_counts[n]);
        char* p = path_of(part);
        assert(bytes_eq((const uint8_t*)p, strlen(p), prefixes[n].ptr));
        free(p);
        kvstore_Store_destroy(part);  // each element is an owned reference
        n++;
    }
    assert(n == 3 && err.code == 0);
    kvstore_Store_PartitionIterator_destroy(pt);
    kvstore_Store_destroy(s);
}

static void listeners(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/listen");

    listener_ctx* l = new_listener("quiet", NULL);
    uint32_t id = kvstore_Store_subscribe(s, listener_arg(l, &LISTENER_VTABLE), &err);
    assert(err.code == 0 && id > 0);
    assert(kvstore_Store_listener_count(s, &err) == 1);

    put(s, "a", "1");
    assert(atomic_load(&l->puts) == 1 && l->last_version == 1 && !l->last_replaced);
    put(s, "a", "2");
    assert(atomic_load(&l->puts) == 2 && l->last_version == 2 && l->last_replaced);
    assert(strcmp(l->last_key, "a") == 0);
    put(s, "quiet", "x");  // accepts() said no
    assert(atomic_load(&l->puts) == 2);
    assert(kvstore_Store_delete(s, str_run("a"), &err));
    assert(atomic_load(&l->removed) == 1 && strcmp(l->last_key, "a") == 0);

    // An expired read removes the entry and says so.
    const int64_t ttl = 1;
    assert(try_put(s, "short", "x", kvstore_EntryKind_Volatile, &ttl, NULL, &err));
    kvstore_Store_tick(s, 1, &err);
    assert(kvstore_Store_get(s, str_run("short"), &err).ptr == NULL);
    assert(err.code == kvstore_KvError_Expired);
    kvstore_error_clear(&err);
    assert(atomic_load(&l->expired) == 1 && strcmp(l->last_key, "short") == 0);

    assert(kvstore_Store_clear(s, &err) == 1);  // "quiet" was left
    assert(atomic_load(&l->cleared) == 1 && l->last_cleared == 1);
    assert(atomic_load(&l->off_main_thread) == 0 && "synchronous calls notify inline");

    // Unsubscribing releases the listener once.
    assert(kvstore_Store_unsubscribe(s, id, &err));
    assert(atomic_load(&g_listeners_freed) == 1);
    assert(!kvstore_Store_unsubscribe(s, id, &err));
    assert(kvstore_Store_listener_count(s, &err) == 0);

    // A listener that fails is detached (and released); the put succeeds.
    listener_ctx* failing = new_listener(NULL, "boom");
    kvstore_Store_subscribe(s, listener_arg(failing, &LISTENER_VTABLE), &err);
    put(s, "fine", "1");
    assert(atomic_load(&failing->puts) == 1);
    put(s, "boom", "1");
    assert(count(s) == 2);
    assert(kvstore_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 2);

    // A vtable shorter than the producer's (compiled before `on_change`
    // was appended) is accepted: `accepts` is called, and the missing
    // `on_change` fails (-4) instead, which detaches and releases the
    // listener without failing the put.
    kvstore_Store_subscribe(s, listener_arg(new_listener(NULL, NULL), &SHORT_LISTENER_VTABLE),
                            &err);
    assert(err.code == 0 && kvstore_Store_listener_count(s, &err) == 1);
    put(s, "open", "1");
    assert(count(s) == 3 && kvstore_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 3);
    assert(atomic_load(&g_freed_accepted) == 1 && atomic_load(&g_freed_puts) == 0);

    // A thread-affine listener: compaction notifies from a producer thread,
    // where the producer refuses to call `accepts` (it returns a value), so
    // the listener fails (-4, never called) and is detached. A synchronous
    // put on this thread still reaches it.
    listener_ctx* affine = new_listener(NULL, NULL);
    kvstore_Store_subscribe(s, listener_arg(affine, &AFFINE_LISTENER_VTABLE), &err);
    assert(err.code == 0 && kvstore_Store_listener_count(s, &err) == 1);
    const int64_t one = 1;
    assert(try_put(s, "brief", "x", kvstore_EntryKind_Volatile, &one, NULL, &err));
    assert(atomic_load(&affine->puts) == 1);
    kvstore_Store_tick(s, 1, &err);
    int off_main = atomic_load(&g_listener_calls_off_main);
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_Store_compact(s, 0, NULL, on_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 1);
    // Detached and released without one call off this thread.
    assert(atomic_load(&g_listener_calls_off_main) == off_main);
    assert(kvstore_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 4);

    // Destroying the store releases the listeners it still holds.
    kvstore_Store_subscribe(s, listener_arg(new_listener(NULL, NULL), &LISTENER_VTABLE), &err);
    kvstore_Store_subscribe(s, listener_arg(new_listener(NULL, NULL), &LISTENER_VTABLE), &err);
    assert(kvstore_Store_listener_count(s, &err) == 2);
    kvstore_Store_destroy(s);
    assert(atomic_load(&g_listeners_freed) == 6);
}

static void policies(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/policy");
    kvstore_Store* other = open_store("/other");

    policy_ctx* p = new_policy(other);
    kvstore_Store_set_policy(s, policy_arg(p, &POLICY_VTABLE), &err);
    assert(err.code == 0 && kvstore_Store_has_policy(s, &err));

    // admit's record return is what's stored (its key and version aside).
    kvstore_Entry e;
    assert(try_put(s, "a", "1", kvstore_EntryKind_Volatile, NULL, &e, &err));
    assert(str_is(e.key, "a") && e.version == 1 && e.kind == kvstore_EntryKind_Encrypted);
    assert(e.tags.len == 1 && str_is(e.tags.items[0], "admitted"));
    kvstore_Entry_free(&e);

    // route: the object parameter and object return redirect a write.
    assert(try_put(s, "b/x", "2", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    assert(count(s) == 1 && count(other) == 1);

    // A typed error from the throwing callback reaches the caller with its
    // code and payload, and the domain's own message for them.
    assert(!try_put(s, "secret", "3", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_KvError_Rejected);
    assert(MSG_EQ(err, "write to secret rejected: no secrets"));
    kvstore_KvError_Rejected_payload r;
    assert(kvstore_KvError_Rejected_payload_decode(err.payload_ptr, err.payload_len, &r));
    assert(str_is(r.key, "secret") && str_is(r.reason, "no secrets"));
    kvstore_KvError_Rejected_payload_free(&r);
    kvstore_error_clear(&err);

    // A code outside the domain reaches the producer as a callback failure,
    // which the sample turns into `CallbackFailed` with the consumer's
    // message.
    assert(!try_put(s, "boom", "4", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, "policy exploded");
    // So is a return the runtime can't accept: a malformed record...
    assert(!try_put(s, "garbage", "5", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, NULL);
    // ...or a null required object.
    assert(!try_put(s, "null/x", "6", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, NULL);
    assert(count(s) == 1 && count(other) == 1);
    assert(atomic_load(&p->admitted) == 6);

    // ttl_for: an optional scalar in and out, consulted before admit.
    kvstore_Entry e2;
    const int64_t five = 5;
    assert(try_put(s, "short", "x", kvstore_EntryKind_Volatile, NULL, &e2, &err));
    assert(e2.expires_at != NULL && *e2.expires_at == 1);
    kvstore_Entry_free(&e2);
    assert(try_put(s, "forever", "x", kvstore_EntryKind_Volatile, &five, &e2, &err));
    assert(e2.expires_at == NULL);
    kvstore_Entry_free(&e2);
    assert(try_put(s, "kept", "x", kvstore_EntryKind_Volatile, &five, &e2, &err));
    assert(e2.expires_at != NULL && *e2.expires_at == 5);
    kvstore_Entry_free(&e2);
    assert(!try_put(s, "ttl-fail", "x", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_KvError_InvalidPath && MSG_EQ(err, "invalid path"));
    kvstore_error_clear(&err);
    assert(count(s) == 4 && atomic_load(&p->admitted) == 9);

    // Replacing the policy releases the old one; a null vtable removes it.
    kvstore_Store_set_policy(s, policy_arg(new_policy(other), &POLICY_VTABLE), &err);
    assert(atomic_load(&g_policies_freed) == 1);
    kvstore_Store_set_policy(s, policy_arg(NULL, NULL), &err);
    assert(err.code == 0 && atomic_load(&g_policies_freed) == 2);
    assert(!kvstore_Store_has_policy(s, &err));
    put(s, "secret", "now allowed");
    assert(count(s) == 5);

    kvstore_Store_destroy(other);
    kvstore_Store_destroy(s);
}

static void loaders(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/load");
    kvstore_Entry* e = NULL;

    // No loader (a null optional callback): a miss is none.
    assert(get_or_load(s, "k", NULL, &e, &err) && e == NULL);

    // load's bytes are stored, tagged with the loader's name.
    assert(get_or_load(s, "k", new_loader(NULL), &e, &err) && e != NULL);
    assert(e->value.len == 8 && memcmp(e->value.ptr, "loaded:k", 8) == 0);
    assert(e->kind == kvstore_EntryKind_Volatile && e->metadata.len == 1);
    assert(str_is(e->metadata.keys[0], "source") && str_is(e->metadata.values[0], "c-loader"));
    kvstore_buf_opt_Entry_free(&e);
    assert(atomic_load(&g_loaders_freed) == 1 && "a loader is released after the call");
    assert(count(s) == 1);
    // A hit doesn't consult the loader.
    assert(get_or_load(s, "k", new_loader(NULL), &e, &err) && e != NULL && e->version == 1);
    kvstore_buf_opt_Entry_free(&e);

    // The fallback store (an optional object return) is consulted first.
    kvstore_Store* backup = open_store("/backup");
    put(backup, "fb", "from backup");
    assert(get_or_load(s, "fb", new_loader(kvstore_Store_clone(backup)), &e, &err));
    assert(e != NULL && e->value.len == 11 && e->kind == kvstore_EntryKind_Persistent);
    kvstore_buf_opt_Entry_free(&e);
    kvstore_Store_destroy(backup);

    // KeyNotFound for this key: the producer decoded the payload and
    // answers none.
    assert(get_or_load(s, "missing", new_loader(NULL), &e, &err) && e == NULL);
    // KeyNotFound for another key: passed through, payload intact.
    assert(!get_or_load(s, "elsewhere", new_loader(NULL), &e, &err));
    assert(MSG_EQ(err, "key not found: other"));
    expect_key_not_found(&err, "other");
    // Any other failure is `CallbackFailed` with the loader's message.
    assert(!get_or_load(s, "broken", new_loader(NULL), &e, &err));
    expect_callback_failed(&err, "loader is broken");

    assert(atomic_load(&g_loaders_freed) == 6);
    kvstore_Store_destroy(s);
}

// Vtables are open: a consumer generated before a method was appended
// passes a shorter vtable (or a null slot), and the producer fails a call
// to the missing method with -4 and a message naming it, without calling
// anything. The sample turns that failure into `CallbackFailed`.
static void open_vtables(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/open");
    put(s, "a", "1");

    // A policy whose vtable ends before `route`: `ttl_for` and `admit` are
    // called, then `route` is missing.
    policy_ctx* p = new_policy(s);
    kvstore_Store_set_policy(s, policy_arg(p, &SHORT_POLICY_VTABLE), &err);
    assert(err.code == 0 && kvstore_Store_has_policy(s, &err));
    assert(!try_put(s, "b", "2", kvstore_EntryKind_Volatile, NULL, NULL, &err));
    expect_missing_method(&err, "route");
    assert(atomic_load(&p->admitted) == 1 && count(s) == 1);
    kvstore_Store_set_policy(s, policy_arg(NULL, NULL), &err);
    assert(atomic_load(&g_policies_freed) == 3);

    // A loader whose `load` slot is null (the vtable is full size):
    // `fallback` is consulted, then `load` is missing.
    kvstore_Entry* e = NULL;
    assert(!get_or_load_with(s, "nl", new_loader(NULL), &NO_LOAD_VTABLE, &e, &err));
    expect_missing_method(&err, "load");
    assert(atomic_load(&g_loaders_freed) == 7);

    // A scorer vtable that is only the header.
    assert(kvstore_Store_rank(s, new_scorer(SCORE_SIZES, &HEADER_ONLY_SCORER_VTABLE), &err).ptr ==
           NULL);
    expect_missing_method(&err, "scores");
    assert(atomic_load(&g_scorers_freed) == 4);

    // A listener whose `on_change` slot is null is detached by the first
    // change it accepts, as if its vtable were short.
    static const kvstore_Listener_vtable no_on_change = {
        sizeof(kvstore_Listener_vtable), 0, listener_free, listener_accepts, NULL,
    };
    kvstore_Store_subscribe(s, listener_arg(new_listener(NULL, NULL), &no_on_change), &err);
    assert(err.code == 0 && kvstore_Store_listener_count(s, &err) == 1);
    put(s, "c", "3");
    assert(kvstore_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 8);
    assert(atomic_load(&g_freed_accepted) == 1 && atomic_load(&g_freed_puts) == 0);

    // A null `free` is skipped: the producer never touches `ctx` again
    // once it releases the listener, so the consumer frees it.
    static const kvstore_Listener_vtable no_free = {
        sizeof(kvstore_Listener_vtable), 0, NULL, listener_accepts, listener_on_change,
    };
    listener_ctx* unowned = new_listener(NULL, NULL);
    uint32_t id = kvstore_Store_subscribe(s, listener_arg(unowned, &no_free), &err);
    put(s, "d", "4");
    assert(atomic_load(&unowned->puts) == 1);
    assert(kvstore_Store_unsubscribe(s, id, &err));
    assert(atomic_load(&g_listeners_freed) == 8);
    free(unowned);

    // A vtable shorter than its header can't be adopted at all: the call
    // that passed it fails with -3 and never calls `free`.
    static const kvstore_Listener_vtable headless = {8, 0, listener_free, NULL, NULL};
    listener_ctx* rejected = new_listener(NULL, NULL);
    assert(kvstore_Store_subscribe(s, listener_arg(rejected, &headless), &err) == 0);
    assert(err.code == -3 && MSG_EQ(err, "listener: a callback vtable is 8 bytes, shorter than its header"));
    kvstore_error_clear(&err);
    assert(kvstore_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 8);
    free(rejected);

    assert(count(s) == 3);
    kvstore_Store_destroy(s);
}

static void async_calls(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/async-calls");
    listener_ctx* l = new_listener(NULL, NULL);
    kvstore_Store_subscribe(s, listener_arg(l, &LISTENER_VTABLE), &err);

    const int64_t ttl = 1;
    assert(try_put(s, "old1", "x", kvstore_EntryKind_Volatile, &ttl, NULL, &err));
    assert(try_put(s, "old2", "x", kvstore_EntryKind_Volatile, &ttl, NULL, &err));
    put(s, "keep", "x");
    kvstore_Store_tick(s, 5, &err);

    // compact runs on a producer thread and notifies listeners there.
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_cancel_token* token = kvstore_cancel_token_create();
    kvstore_Store_compact(s, 0, token, on_u32, &c);
    kvstore_cancel_token_destroy(token);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 2);
    assert(atomic_load(&l->expired) == 2);
    assert(atomic_load(&l->off_main_thread) == 2 && "notified from a producer thread");
    assert(count(s) == 1);

    // A null token never cancels.
    memset(&c, 0, sizeof c);
    kvstore_Store_compact(s, 5, NULL, on_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 0);

    // Cancel mid-pause: the call completes with -5 at once, and the
    // background pause notices the token and stops.
    memset(&c, 0, sizeof c);
    token = kvstore_cancel_token_create();
    kvstore_Store_compact(s, 60000, token, on_u32, &c);
    sleep_ms(20);
    assert(!atomic_load(&c.done));
    assert(kvstore_Store_active_jobs(&err) >= 1);
    kvstore_cancel_token_cancel(token);
    kvstore_cancel_token_destroy(token);
    wait_for(&c);
    assert(c.code == -5 && strcmp(c.message, "cancelled") == 0 && c.u32 == 0);
    int stopped = 0;
    for (int i = 0; i < 2000 && !stopped; i++) {
        stopped = kvstore_Store_active_jobs(&err) == 0;
        if (!stopped) sleep_ms(1);
    }
    assert(stopped && "the cancelled pause stopped cooperatively");

    // get_many: an async list of optional records, launched concurrently.
    kvstore_str keys[3] = {kvstore_str_of("keep"), kvstore_str_of("gone"),
                           kvstore_str_of("keep")};
    kvstore_buf_list_string kl;
    kl.items = keys;
    kl.len = 3;
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_string_write(&w, &kl);
    enum { CONCURRENT = 32 };
    static call_state many[CONCURRENT];
    memset(many, 0, sizeof many);
    for (int i = 0; i < CONCURRENT; i++) {
        kvstore_Store_get_many(s, kvstore_writer_run(&w), on_run, &many[i]);
    }
    kvstore_writer_free(&w);
    for (int i = 0; i < CONCURRENT; i++) {
        wait_for(&many[i]);
        assert(many[i].code == 0);
        kvstore_buf_list_opt_Entry got;
        assert(kvstore_buf_list_opt_Entry_decode(many[i].buf, many[i].len, &got));
        assert(got.len == 3 && got.items[0] != NULL && got.items[1] == NULL);
        assert(got.items[2] != NULL && str_is(got.items[2]->key, "keep"));
        kvstore_buf_list_opt_Entry_free(&got);
        free(many[i].buf);
    }

    // summarize_all (from another Rust module): objects in a list in, a
    // record out.
    kvstore_Store* other = open_store("/other");
    put(other, "a", "123");
    kvstore_Store* both[2] = {s, other};
    kvstore_buf_list_Store sl;
    sl.items = both;
    sl.len = 2;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_Store_write(&w, &sl);
    memset(&c, 0, sizeof c);
    kvstore_summarize_all(kvstore_writer_run(&w), on_run, &c);
    kvstore_writer_free(&w);
    wait_for(&c);
    assert(c.code == 0);
    kvstore_Stats st;
    assert(kvstore_Stats_decode(c.buf, c.len, &st));
    free(c.buf);
    assert(st.entries == 2 && st.bytes == 4);
    assert(st.by_kind.len == 1 && st.by_kind.keys[0] == kvstore_EntryKind_Persistent);
    assert(st.by_kind.values[0] == 2);
    kvstore_Stats_free(&st);

    kvstore_Store_destroy(other);
    kvstore_Store_destroy(s);
}

static kvstore_StoreInfo describe(kvstore_Store* s, const char* label,
                                  const kvstore_Store* mirror) {
    kvstore_error err;
    kvstore_run buf = kvstore_Store_describe(s, str_run(label), mirror, &err);
    assert(err.code == 0 && buf.ptr != NULL);
    kvstore_StoreInfo info;
    assert(kvstore_StoreInfo_take(buf, &info));
    return info;
}

static void object_graph(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/graph");
    put(s, "k", "v");

    // share(): the same object; clone/destroy count references.
    kvstore_Store* shared = kvstore_Store_share(s, &err);
    assert(err.code == 0 && shared == s);
    kvstore_Store_destroy(s);
    assert(count(shared) == 1 && "alive through the shared reference");
    s = shared;
    kvstore_Store* cloned = kvstore_Store_clone(s);
    assert(cloned == s);
    kvstore_Store_destroy(cloned);
    assert(kvstore_Store_clone(NULL) == NULL);
    kvstore_Store_destroy(NULL);

    // fork(): a distinct object with a copy of the entries.
    kvstore_Store* fork = kvstore_Store_fork(s, &err);
    assert(fork != NULL && fork != s && count(fork) == 1 && path_is(fork, "/graph"));
    put(fork, "k2", "v");
    assert(count(fork) == 2 && count(s) == 1);

    // larger(): `Store?` in and out.
    kvstore_Store* empty = open_store("/empty");
    assert(kvstore_Store_larger(empty, NULL, &err) == NULL && err.code == 0);
    kvstore_Store* bigger = kvstore_Store_larger(empty, fork, &err);
    assert(bigger == fork);
    kvstore_Store_destroy(bigger);
    bigger = kvstore_Store_larger(s, NULL, &err);
    assert(bigger == s);
    kvstore_Store_destroy(bigger);

    // describe(): a record whose fields carry objects.
    kvstore_StoreInfo info = describe(s, "main", fork);
    assert(str_is(info.label, "main") && info.store == s && info.mirror == fork);
    assert(info.count == 1 && count(info.mirror) == 2);

    // open_many(): a list of objects; one bad path fails the whole call.
    kvstore_str paths[2] = {kvstore_str_of("/a"), kvstore_str_of("/b")};
    kvstore_buf_list_string pl;
    pl.items = paths;
    pl.len = 2;
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_string_write(&w, &pl);
    kvstore_run buf = kvstore_Store_open_many(kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    kvstore_buf_list_Store many;
    assert(err.code == 0 && kvstore_buf_list_Store_take(buf, &many));
    assert(many.len == 2 && path_is(many.items[0], "/a") && path_is(many.items[1], "/b"));
    paths[1] = kvstore_str_of("");
    memset(&w, 0, sizeof w);
    kvstore_buf_list_string_write(&w, &pl);
    assert(kvstore_Store_open_many(kvstore_writer_run(&w), &err).ptr == NULL);
    kvstore_writer_free(&w);
    assert(err.code == kvstore_KvError_InvalidPath);
    kvstore_error_clear(&err);

    // by_label(): records with objects in, a map with object values out.
    kvstore_StoreInfo infos[2];
    infos[0] = info;
    infos[1].label = kvstore_str_of("first");
    infos[1].store = many.items[0];
    infos[1].mirror = NULL;
    infos[1].count = 0;
    kvstore_buf_list_StoreInfo il;
    il.items = infos;
    il.len = 2;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_StoreInfo_write(&w, &il);
    buf = kvstore_Store_by_label(kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    kvstore_buf_map_string_Store named;
    assert(err.code == 0 && kvstore_buf_map_string_Store_take(buf, &named));
    assert(named.len == 2);
    for (size_t i = 0; i < named.len; i++) {
        if (str_is(named.keys[i], "main")) assert(named.values[i] == s);
        else assert(str_is(named.keys[i], "first") && named.values[i] == many.items[0]);
    }

    // total_count(): a list, a map, and an optional record, all carrying
    // objects (each written as a fresh reference the producer adopts).
    put(many.items[0], "m", "1");
    kvstore_Store* three[3] = {many.items[0], many.items[1], fork};
    kvstore_buf_list_Store stores;
    stores.items = three;
    stores.len = 3;
    kvstore_writer ws, wn, we;
    memset(&ws, 0, sizeof ws);
    memset(&wn, 0, sizeof wn);
    memset(&we, 0, sizeof we);
    kvstore_buf_list_Store_write(&ws, &stores);
    kvstore_buf_map_string_Store_write(&wn, &named);
    kvstore_buf_opt_StoreInfo_write(&we, &info);
    // stores: 1 + 0 + 2; named: main 1 + first 1; extra: main 1.
    assert(kvstore_Store_total_count(kvstore_writer_run(&ws), kvstore_writer_run(&wn),
                                     kvstore_writer_run(&we), &err) == 6);
    assert(err.code == 0);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&wn);
    kvstore_writer_free(&we);
    // A buffer carrying objects is consumed by the call that receives it
    // (its references are adopted), so each call gets fresh buffers.
    memset(&ws, 0, sizeof ws);
    memset(&wn, 0, sizeof wn);
    memset(&we, 0, sizeof we);
    kvstore_buf_list_Store_write(&ws, &stores);
    kvstore_buf_map_string_Store_write(&wn, &named);
    kvstore_buf_opt_StoreInfo_write(&we, NULL);
    assert(kvstore_Store_total_count(kvstore_writer_run(&ws), kvstore_writer_run(&wn),
                                     kvstore_writer_run(&we), &err) == 5);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&wn);
    kvstore_writer_free(&we);

    // Everything we hold is still intact; release each reference once.
    assert(count(s) == 1 && count(fork) == 2 && count(many.items[0]) == 1);
    kvstore_buf_map_string_Store_free(&named);
    kvstore_StoreInfo_free(&info);
    kvstore_buf_list_Store_free(&many);
    kvstore_Store_destroy(empty);
    kvstore_Store_destroy(fork);
    kvstore_Store_destroy(s);
}

// `summarize` and `render_report` are declared in the sample's other Rust
// modules; they share the one namespace, the `Store`, the `Entry` record,
// and `KvError`, and `render_report` has its own error domain.
static void stats_and_report(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/stats");
    put(s, "b", "12");
    put(s, "a", "1");
    put(s, "a", "123");
    assert(try_put(s, "c", "x", kvstore_EntryKind_Encrypted, NULL, NULL, &err));

    // summarize: the Store as a parameter, KvError for a prefix that
    // matches nothing.
    kvstore_writer w = prefix_buf(NULL);
    kvstore_run buf = kvstore_summarize(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    kvstore_Stats st;
    assert(err.code == 0 && kvstore_Stats_take(buf, &st));
    assert(st.entries == 3 && st.bytes == 6 && st.by_kind.len == 2);
    for (size_t i = 0; i < st.by_kind.len; i++) {
        if (st.by_kind.keys[i] == kvstore_EntryKind_Persistent) assert(st.by_kind.values[i] == 2);
        else assert(st.by_kind.keys[i] == kvstore_EntryKind_Encrypted && st.by_kind.values[i] == 1);
    }
    kvstore_Stats_free(&st);
    w = prefix_buf("q");
    assert(kvstore_summarize(s, kvstore_writer_run(&w), &err).ptr == NULL);
    kvstore_writer_free(&w);
    expect_key_not_found(&err, "q");

    // render_report: a list of the shared Entry record.
    w = prefix_buf(NULL);
    kvstore_Store_EntriesIterator* it = kvstore_Store_entries(s, kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    kvstore_Entry entries[3];
    kvstore_run item;
    size_t n = 0;
    while (kvstore_Store_EntriesIterator_next(it, &item, &err) == 1) {
        assert(n < 3 && kvstore_Entry_take(item, &entries[n++]));
    }
    kvstore_Store_EntriesIterator_destroy(it);
    assert(n == 3);
    kvstore_buf_list_Entry el;
    el.items = entries;
    el.len = n;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_Entry_write(&w, &el);
    buf = kvstore_render_report(kvstore_writer_run(&w), &err);
    kvstore_writer_free(&w);
    kvstore_buf_list_string lines;
    assert(err.code == 0 && kvstore_buf_list_string_take(buf, &lines));
    assert(lines.len == 3);
    assert(str_is(lines.items[0], "a: 3 bytes, Persistent, v2"));
    assert(str_is(lines.items[1], "b: 2 bytes, Persistent"));
    assert(str_is(lines.items[2], "c: 1 bytes, Encrypted"));
    kvstore_buf_list_string_free(&lines);
    for (size_t i = 0; i < n; i++) kvstore_Entry_free(&entries[i]);

    el.len = 0;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_Entry_write(&w, &el);
    assert(kvstore_render_report(kvstore_writer_run(&w), &err).ptr == NULL);
    kvstore_writer_free(&w);
    assert(err.code == kvstore_ReportError_NothingToReport);
    assert(MSG_EQ(err, "nothing to report"));
    kvstore_error_clear(&err);
    kvstore_Store_destroy(s);
}

// One C value per value: optional scalars as opt structs and typed arrays
// as runs (`len` in bytes) in every position, a `usize`, and `throws: any`.
static void value_shapes(void) {
    kvstore_error err;
    kvstore_Store* s = open_store("/shapes");

    const int64_t seven = 7;
    assert(try_put(s, "b", "12", kvstore_EntryKind_Persistent, NULL, NULL, &err));
    assert(try_put(s, "a", "\x01\x02\x03", kvstore_EntryKind_Volatile, &seven, NULL, &err));
    assert(count(s) == 2);

    // An optional scalar return (`value` is 0 when absent).
    kvstore_opt_i64 at = kvstore_Store_expires_at(s, str_run("a"), &err);
    assert(at.has && at.value == 7 && err.code == 0);
    at = kvstore_Store_expires_at(s, str_run("b"), &err);
    assert(!at.has && at.value == 0 && err.code == 0);
    at = kvstore_Store_expires_at(s, str_run("zzz"), &err);
    assert(!at.has && at.value == 0 && err.code == 0);

    // A typed-array return, in key order.
    kvstore_run sizes = kvstore_Store_value_sizes(s, &err);
    assert(err.code == 0 && sizes.len == 2 * sizeof(uint64_t) && ((uintptr_t)sizes.ptr % 8) == 0);
    const uint64_t* size_of = (const uint64_t*)(const void*)sizes.ptr;
    assert(size_of[0] == 3 && size_of[1] == 2);
    kvstore_run_free(sizes);

    // Optional scalar iterator items.
    kvstore_Store_ExpirationsIterator* it = kvstore_Store_expirations(s, &err);
    assert(err.code == 0 && it != NULL);
    kvstore_opt_i64 item;
    assert(kvstore_Store_ExpirationsIterator_next(it, &item, &err) == 1);
    assert(item.has && item.value == 7);
    assert(kvstore_Store_ExpirationsIterator_next(it, &item, &err) == 1);
    assert(!item.has && item.value == 0);
    assert(kvstore_Store_ExpirationsIterator_next(it, &item, &err) == 0 && err.code == 0);
    kvstore_Store_ExpirationsIterator_destroy(it);

    // An optional scalar async result.
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_Store_version_of(s, str_run("a"), on_opt_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 1);
    memset(&c, 0, sizeof c);
    kvstore_Store_version_of(s, str_run("q"), on_opt_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == UINT32_MAX);

    // A typed-array async result.
    put(s, "b", "x");
    kvstore_str keys[3] = {kvstore_str_of("b"), kvstore_str_of("q"), kvstore_str_of("a")};
    kvstore_buf_list_string kl = {keys, 3};
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_buf_list_string_write(&w, &kl);
    memset(&c, 0, sizeof c);
    kvstore_Store_versions(s, kvstore_writer_run(&w), on_run, &c);
    kvstore_writer_free(&w);
    wait_for(&c);
    assert(c.code == 0 && c.len == 3 * sizeof(uint32_t));
    uint32_t versions[3];
    memcpy(versions, c.buf, sizeof versions);
    assert(versions[0] == 2 && versions[1] == 0 && versions[2] == 1);
    free(c.buf);
    kvstore_Store_destroy(s);

    // A callback taking and returning typed arrays.
    s = open_store("/rank");
    put(s, "a", "1");
    put(s, "b", "333");
    put(s, "c", "22");
    kvstore_buf_list_string ranked;
    kvstore_run buf = kvstore_Store_rank(s, new_scorer(SCORE_SIZES, &SCORER_VTABLE), &err);
    assert(err.code == 0 && kvstore_buf_list_string_take(buf, &ranked));
    assert(ranked.len == 3 && str_is(ranked.items[0], "b") && str_is(ranked.items[1], "c"));
    assert(str_is(ranked.items[2], "a"));
    kvstore_buf_list_string_free(&ranked);
    assert(kvstore_Store_rank(s, new_scorer(SCORE_FAIL, &SCORER_VTABLE), &err).ptr == NULL);
    expect_callback_failed(&err, "scorer is out of order");
    assert(kvstore_Store_rank(s, new_scorer(SCORE_SHORT, &SCORER_VTABLE), &err).ptr == NULL);
    expect_callback_failed(&err, "expected 3 scores, got 1");
    kvstore_Store_destroy(s);

    // `throws: any` and a `usize` return: code -1 and a message, no payload.
    s = open_store("/import");
    assert(kvstore_Store_import_lines(s, str_run("a=1\n\nb=two\n"), &err) == 2 && err.code == 0);
    buf = kvstore_Store_get(s, str_run("b"), &err);
    kvstore_Entry e;
    assert(err.code == 0 && kvstore_Entry_take(buf, &e));
    assert(e.value.len == 3 && memcmp(e.value.ptr, "two", 3) == 0);
    kvstore_Entry_free(&e);
    assert(kvstore_Store_import_lines(s, str_run("c=3\nbroken\nd=4"), &err) == 0);
    assert(err.code == -1 && MSG_EQ(err, "line 2: expected key=value"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    kvstore_error_clear(&err);
    assert(count(s) == 3);
    kvstore_Store_destroy(s);
}

int main(void) {
    g_main_thread = pthread_self();

    // `out_err` is overwritten, never read: garbage in it is fine.
    kvstore_error err;
    memset(&err, 0xA5, sizeof err);

    assert(KVSTORE_ABI_VERSION == 6u && kvstore_abi_version() == KVSTORE_ABI_VERSION);
    contracts();
    assert(kvstore_debug_live(-1) == 1 && "the sample counts live allocations");
    assert(kvstore_Store_default_capacity(&err) == 1000000 && err.code == 0);

    constructors();
    basics();
    iterators();
    listeners();
    policies();
    loaders();
    async_calls();
    object_graph();
    stats_and_report();
    value_shapes();
    open_vtables();

    ASSERT_NO_LEAKS(kvstore_debug_live);
    assert(atomic_load(&g_listeners_freed) == 8);
    assert(atomic_load(&g_policies_freed) == 3);
    assert(atomic_load(&g_loaders_freed) == 7);
    assert(atomic_load(&g_scorers_freed) == 4);
    printf("c/kvstore: OK\n");
    return 0;
}
