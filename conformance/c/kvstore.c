// Conformance consumer: kvstore sample, C target (ABI revision 5).
//
// Drives the feature-complete producer through its generated header and
// `kvstore_buffer.h`:
//
//   * the load-time checks (ABI revision, both modules' contract tables);
//   * the `Store` interface: fallible and infallible constructors, methods,
//     statics, the deprecated `size`, records (`Entry`, `StoreInfo`), the
//     C-style `EntryKind`, maps and optionals, and the logical clock;
//   * `KvError` codes with their payload fields decoded (`KeyNotFound`,
//     `Expired`, `StoreFull`, `Rejected`), plus marshalling failures (-3);
//   * lazy iterators of strings (throwing), records, and objects;
//   * three callback interfaces implemented in C: a `Listener` (retained,
//     filtered by `accepts`, told about every `Change`, detached when it
//     fails, and notified from a producer thread during compaction), a
//     `Policy` (a record return, a throwing method whose typed error and
//     payload reach the `put` caller, an object parameter and object
//     return, a null object and a malformed return rejected), and a `Loader`
//     passed as an optional callback (string, bytes, and optional-object
//     returns; typed errors decoded by the producer or passed through);
//   * `Store` objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * async calls: an async free function returning an object, a cancellable
//     method cancelled mid-pause (completing with -5 while its background
//     work stops cooperatively, shown by `active_jobs`), an async list, an
//     async free function in the nested `kv.stats` module, and concurrent
//     launches;
//   * the nested `kv.stats` module (the parent's `Store` and error domain)
//     and the sibling `report` root (the shared `Entry` record and its own
//     error domain);
//   * the ABI 5 shapes: an optional scalar parameter, return, iterator item,
//     async result, and callback parameter and return (OptDirect); typed
//     arrays as a return, an async result, and a callback parameter and
//     return (Slice); a `usize` return; a `throws: any` method; a callback
//     failure converted into the domain's `CallbackFailed`; and a
//     thread-affine vtable whose value-returning method the producer refuses
//     to call off its thread.
//
// Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include <pthread.h>
#include <stdatomic.h>

#include "kvstore_buffer.h"

static pthread_t g_main_thread;

// ── small helpers ──────────────────────────────────────────────────────────

static int str_is(kvstore_str s, const char* expected) {
    return bytes_eq((const uint8_t*)s.ptr, s.len, expected);
}

static int starts_with(kvstore_str s, const char* prefix) {
    size_t n = strlen(prefix);
    return s.len >= n && memcmp(s.ptr, prefix, n) == 0;
}

static char* take_string(const uint8_t* ptr, size_t len) {
    char* s = (char*)calloc(len + 1, 1);
    if (len > 0) memcpy(s, ptr, len);
    kvstore_free_bytes((uint8_t*)ptr, len);
    return s;
}

// Copy a writer's bytes into a run the producer adopts.
static void hand_over(kvstore_writer* w, uint8_t** out_ptr, size_t* out_len) {
    assert(!w->failed);
    uint8_t* run = kvstore_alloc(w->len);
    if (w->len > 0) memcpy(run, w->ptr, w->len);
    *out_ptr = run;
    *out_len = w->len;
    kvstore_writer_free(w);
}

static void hand_over_text(const char* s, uint8_t** out_ptr, size_t* out_len) {
    size_t n = strlen(s);
    uint8_t* run = kvstore_alloc(n);
    if (n > 0) memcpy(run, s, n);
    *out_ptr = run;
    *out_len = n;
}

static kvstore_kv_Store* open_store(const char* path) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = kvstore_kv_Store_open(STR(path), &err);
    assert(err.code == 0 && s != NULL);
    return s;
}

static uint64_t count(const kvstore_kv_Store* s) {
    kvstore_error err = {0};
    uint64_t n = kvstore_kv_Store_count(s, &err);
    assert(err.code == 0);
    return n;
}

static char* path_of(const kvstore_kv_Store* s) {
    kvstore_error err = {0};
    size_t len = 0;
    const uint8_t* p = kvstore_kv_Store_path(s, &len, &err);
    assert(err.code == 0);
    return take_string(p, len);
}

static int path_is(const kvstore_kv_Store* s, const char* expected) {
    char* p = path_of(s);
    int ok = strcmp(p, expected) == 0;
    free(p);
    return ok;
}

// put(key, value, kind, ttl?): decodes the returned entry into `out` on
// success; on failure leaves the error in `err`.
static bool try_put(const kvstore_kv_Store* s, const char* key, const char* value,
                    kvstore_kv_EntryKind kind, const int64_t* ttl, kvstore_kv_Entry* out,
                    kvstore_error* err) {
    size_t len = 0;
    const uint8_t* buf = kvstore_kv_Store_put(s, STR(key), (const uint8_t*)value, strlen(value),
                                              kind, ttl != NULL, ttl != NULL ? *ttl : 0, &len, err);
    if (buf == NULL) {
        assert(err->code != 0);
        return false;
    }
    assert(err->code == 0);
    kvstore_kv_Entry e;
    assert(kvstore_kv_Entry_decode(buf, len, &e));
    kvstore_free_bytes((uint8_t*)buf, len);
    if (out != NULL) {
        *out = e;
    } else {
        kvstore_kv_Entry_free(&e);
    }
    return true;
}

static void put(const kvstore_kv_Store* s, const char* key, const char* value) {
    kvstore_error err = {0};
    assert(try_put(s, key, value, kvstore_kv_EntryKind_Persistent, NULL, NULL, &err));
}

// The optional-string parameter buffer for a prefix.
static kvstore_writer prefix_buf(const char* prefix) {
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_str view = kvstore_str_of(prefix);
    kvstore_opt_string_write(&w, prefix ? &view : NULL);
    return w;
}

static void expect_key_not_found(kvstore_error* err, const char* key) {
    assert(err->code == kvstore_kv_KvError_KeyNotFound);
    kvstore_kv_KvError_KeyNotFound_payload p;
    assert(kvstore_kv_KvError_KeyNotFound_payload_decode(err->payload_ptr, err->payload_len, &p));
    assert(str_is(p.key, key));
    kvstore_kv_KvError_KeyNotFound_payload_free(&p);
    kvstore_error_clear(err);
}

// `CallbackFailed` (a consumer callback's failure, converted by the sample),
// with the consumer's message when `message` isn't NULL.
static void expect_callback_failed(kvstore_error* err, const char* message) {
    assert(err->code == kvstore_kv_KvError_CallbackFailed);
    kvstore_kv_KvError_CallbackFailed_payload p;
    assert(kvstore_kv_KvError_CallbackFailed_payload_decode(err->payload_ptr, err->payload_len,
                                                            &p));
    assert(p.message.len > 0);
    if (message != NULL) {
        assert(MSG_EQ(*err, message) && str_is(p.message, message));
    }
    kvstore_kv_KvError_CallbackFailed_payload_free(&p);
    kvstore_error_clear(err);
}

// ── listener (consumer-implemented, retained) ──────────────────────────────

typedef struct {
    const char* skip;     // accepts() answers false for this key
    const char* fail_on;  // accepts() reports a failure for this key
    atomic_int puts, removed, expired, cleared;
    atomic_int off_main_thread;  // on_change calls on a producer thread
    // The last Put's entry fields and the last Removed key.
    uint32_t last_version;
    bool last_replaced;
    char last_key[64];
    uint32_t last_cleared;
} listener_ctx;

static atomic_int g_listeners_freed;
// Every listener's calls made off the main thread, accepts and on_change.
static atomic_int g_listener_calls_off_main;

static bool listener_accepts(void* ctx, const uint8_t* key_ptr, size_t key_len,
                             kvstore_error* out_err) {
    listener_ctx* l = (listener_ctx*)ctx;
    if (!pthread_equal(pthread_self(), g_main_thread)) atomic_fetch_add(&g_listener_calls_off_main, 1);
    if (l->fail_on != NULL && bytes_eq(key_ptr, key_len, l->fail_on)) {
        kvstore_error_set(out_err, -1, STR("listener refused"));
        return false;
    }
    return l->skip == NULL || !bytes_eq(key_ptr, key_len, l->skip);
}

// The Change buffer is borrowed for the call.
static void listener_on_change(void* ctx, const uint8_t* change_ptr, size_t change_len,
                               kvstore_error* out_err) {
    (void)out_err;
    listener_ctx* l = (listener_ctx*)ctx;
    if (!pthread_equal(pthread_self(), g_main_thread)) {
        atomic_fetch_add(&l->off_main_thread, 1);
        atomic_fetch_add(&g_listener_calls_off_main, 1);
    }
    kvstore_kv_Change c;
    assert(kvstore_kv_Change_decode(change_ptr, change_len, &c));
    switch (c.tag) {
    case kvstore_kv_Change_Put:
        l->last_version = c.as.Put.entry.version;
        l->last_replaced = c.as.Put.replaced;
        snprintf(l->last_key, sizeof l->last_key, "%s", c.as.Put.entry.key.ptr);
        atomic_fetch_add(&l->puts, 1);
        break;
    case kvstore_kv_Change_Removed:
        snprintf(l->last_key, sizeof l->last_key, "%s", c.as.Removed.key.ptr);
        atomic_fetch_add(c.as.Removed.expired ? &l->expired : &l->removed, 1);
        break;
    case kvstore_kv_Change_Cleared:
        l->last_cleared = c.as.Cleared.count;
        atomic_fetch_add(&l->cleared, 1);
        break;
    }
    kvstore_kv_Change_free(&c);
}

static void listener_free(void* ctx) {
    free(ctx);
    atomic_fetch_add(&g_listeners_freed, 1);
}

static const kvstore_kv_Listener_vtable LISTENER_VTABLE = {
    sizeof(kvstore_kv_Listener_vtable), 0, listener_free, listener_accepts, listener_on_change,
};

// The same listener, declared thread-affine: the producer calls `accepts`
// (which returns a value) only on the thread that subscribed it, and fails
// such a call from any other thread with -4 instead of making it.
static const kvstore_kv_Listener_vtable AFFINE_LISTENER_VTABLE = {
    sizeof(kvstore_kv_Listener_vtable), KVSTORE_VTABLE_THREAD_AFFINE, listener_free,
    listener_accepts, listener_on_change,
};

static listener_ctx* new_listener(const char* skip, const char* fail_on) {
    listener_ctx* l = (listener_ctx*)calloc(1, sizeof *l);
    assert(l != NULL);
    l->skip = skip;
    l->fail_on = fail_on;
    return l;
}

// ── policy (consumer-implemented, rich returns, throws) ────────────────────

typedef struct {
    kvstore_kv_Store* other;  // owned reference: where "b/" keys go
    atomic_int admitted;
} policy_ctx;

static atomic_int g_policies_freed;

// ttl_for: an optional scalar parameter and return. "short" lives one tick,
// "forever" never expires, "ttl-fail" fails with a typed error, and every
// other key keeps the requested TTL.
static bool policy_ttl_for(void* ctx, const uint8_t* key_ptr, size_t key_len, bool has_requested,
                           int64_t requested, int64_t* out_value, kvstore_error* out_err) {
    (void)ctx;
    if (bytes_eq(key_ptr, key_len, "short")) {
        *out_value = 1;
        return true;
    }
    if (bytes_eq(key_ptr, key_len, "forever")) return false;
    if (bytes_eq(key_ptr, key_len, "ttl-fail")) {
        kvstore_error_set(out_err, kvstore_kv_KvError_InvalidPath, STR("no ttl for you"));
        return false;
    }
    *out_value = requested;
    return has_requested;
}

static void policy_admit(void* ctx, const uint8_t* entry_ptr, size_t entry_len,
                         uint8_t** out_ptr, size_t* out_len, kvstore_error* out_err) {
    policy_ctx* p = (policy_ctx*)ctx;
    atomic_fetch_add(&p->admitted, 1);
    kvstore_kv_Entry e;
    assert(kvstore_kv_Entry_decode(entry_ptr, entry_len, &e));
    assert(e.version == 0 && "the store assigns the version after admission");
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    if (starts_with(e.key, "secret")) {
        // A typed domain error with its fields as the payload.
        kvstore_error_set(out_err, kvstore_kv_KvError_Rejected, STR("secrets are not stored"));
        kvstore_kv_KvError_Rejected_payload r;
        r.key = e.key;
        r.reason = kvstore_str_of("no secrets");
        kvstore_kv_KvError_Rejected_payload_write(&w, &r);
        kvstore_error_set_payload(out_err, w.ptr, w.len);
        kvstore_writer_free(&w);
    } else if (starts_with(e.key, "boom")) {
        kvstore_error_set(out_err, 4242, STR("policy exploded"));  // not a domain code
    } else if (starts_with(e.key, "garbage")) {
        hand_over_text("?", out_ptr, out_len);  // not an Entry: -3
    } else {
        // Rewrite: tag it and store it encrypted (and try to rename it,
        // which the store ignores).
        kvstore_list_string original_tags = e.tags;
        kvstore_str original_key = e.key;
        kvstore_str tags[1] = {kvstore_str_of("admitted")};
        e.tags.items = tags;
        e.tags.len = 1;
        e.kind = kvstore_kv_EntryKind_Encrypted;
        e.key = kvstore_str_of("renamed");
        kvstore_kv_Entry_write(&w, &e);
        e.tags = original_tags;
        e.key = original_key;
        hand_over(&w, out_ptr, out_len);
    }
    kvstore_kv_Entry_free(&e);
}

// `home` arrives as one strong reference this policy owns; the return is one
// strong reference the producer adopts.
static kvstore_kv_Store* policy_route(void* ctx, const uint8_t* key_ptr, size_t key_len,
                                      kvstore_kv_Store* home, kvstore_error* out_err) {
    (void)out_err;
    policy_ctx* p = (policy_ctx*)ctx;
    kvstore_str key = {(const char*)key_ptr, key_len};
    if (starts_with(key, "b/")) {
        kvstore_kv_Store_destroy(home);
        return kvstore_kv_Store_clone(p->other);
    }
    if (starts_with(key, "null/")) {
        kvstore_kv_Store_destroy(home);
        return NULL;  // a required object may not be null: -3
    }
    return home;
}

static void policy_free(void* ctx) {
    policy_ctx* p = (policy_ctx*)ctx;
    kvstore_kv_Store_destroy(p->other);
    free(p);
    atomic_fetch_add(&g_policies_freed, 1);
}

static const kvstore_kv_Policy_vtable POLICY_VTABLE = {
    sizeof(kvstore_kv_Policy_vtable), 0, policy_free, policy_ttl_for, policy_admit, policy_route,
};

// ── loader (consumer-implemented, passed as an optional parameter) ─────────

typedef struct {
    kvstore_kv_Store* fallback;  // owned reference or NULL
} loader_ctx;

static atomic_int g_loaders_freed;

static void loader_name(void* ctx, uint8_t** out_ptr, size_t* out_len, kvstore_error* out_err) {
    (void)ctx;
    (void)out_err;
    hand_over_text("c-loader", out_ptr, out_len);
}

static kvstore_kv_Store* loader_fallback(void* ctx, const uint8_t* key_ptr, size_t key_len,
                                         kvstore_error* out_err) {
    (void)out_err;
    loader_ctx* l = (loader_ctx*)ctx;
    if (l->fallback != NULL && bytes_eq(key_ptr, key_len, "fb")) {
        return kvstore_kv_Store_clone(l->fallback);
    }
    return NULL;  // `Store?`: none
}

static void fail_not_found(kvstore_error* out_err, const char* key) {
    kvstore_error_set(out_err, kvstore_kv_KvError_KeyNotFound, STR("not in the loader"));
    kvstore_kv_KvError_KeyNotFound_payload p;
    p.key = kvstore_str_of(key);
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_kv_KvError_KeyNotFound_payload_write(&w, &p);
    kvstore_error_set_payload(out_err, w.ptr, w.len);
    kvstore_writer_free(&w);
}

static void loader_load(void* ctx, const uint8_t* key_ptr, size_t key_len, uint8_t** out_ptr,
                        size_t* out_len, kvstore_error* out_err) {
    (void)ctx;
    if (bytes_eq(key_ptr, key_len, "missing")) {
        fail_not_found(out_err, "missing");  // this key: get_or_load returns none
        return;
    }
    if (bytes_eq(key_ptr, key_len, "elsewhere")) {
        fail_not_found(out_err, "other");  // another key: passed through
        return;
    }
    if (bytes_eq(key_ptr, key_len, "broken")) {
        kvstore_error_set(out_err, -1, STR("loader is broken"));
        return;
    }
    char value[80];
    snprintf(value, sizeof value, "loaded:%.*s", (int)key_len, (const char*)key_ptr);
    hand_over_text(value, out_ptr, out_len);
}

static void loader_free(void* ctx) {
    loader_ctx* l = (loader_ctx*)ctx;
    kvstore_kv_Store_destroy(l->fallback);
    free(l);
    atomic_fetch_add(&g_loaders_freed, 1);
}

static const kvstore_kv_Loader_vtable LOADER_VTABLE = {
    sizeof(kvstore_kv_Loader_vtable), 0, loader_free, loader_name, loader_fallback, loader_load,
};

static loader_ctx* new_loader(kvstore_kv_Store* fallback) {
    loader_ctx* l = (loader_ctx*)calloc(1, sizeof *l);
    assert(l != NULL);
    l->fallback = fallback;
    return l;
}

// get_or_load through a fresh loader (or none); decodes `Entry?` into `out`
// (NULL when absent) and returns false with `err` set on failure.
static bool get_or_load(const kvstore_kv_Store* s, const char* key, loader_ctx* loader,
                        kvstore_kv_Entry** out, kvstore_error* err) {
    size_t len = 0;
    const uint8_t* buf = kvstore_kv_Store_get_or_load(
        s, STR(key), loader, loader ? &LOADER_VTABLE : NULL, &len, err);
    if (err->code != 0) {
        assert(buf == NULL);
        return false;
    }
    assert(kvstore_opt_Entry_decode(buf, len, out));
    kvstore_free_bytes((uint8_t*)buf, len);
    return true;
}

// ── async completion state ─────────────────────────────────────────────────

typedef struct {
    atomic_int done;
    int32_t code;
    char message[128];
    uint32_t u32;
    kvstore_kv_Store* store;
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

static void on_store(void* context, kvstore_error* err, kvstore_kv_Store* result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    c->store = result;
    atomic_store(&c->done, 1);
}

// A buffered async result is an owned run: copy it, then release it.
static void on_buffer(void* context, kvstore_error* err, const uint8_t* ptr, size_t len) {
    call_state* c = (call_state*)context;
    settle(c, err);
    if (ptr != NULL) {
        c->buf = (uint8_t*)malloc(len);
        memcpy(c->buf, ptr, len);
        c->len = len;
        kvstore_free_bytes((uint8_t*)ptr, len);
    }
    atomic_store(&c->done, 1);
}

static void wait_for(call_state* c) {
    for (int i = 0; i < 5000 && !atomic_load(&c->done); i++) sleep_ms(1);
    assert(atomic_load(&c->done) && "async call completed");
}

// ── sections ───────────────────────────────────────────────────────────────

static void constructors(void) {
    kvstore_error err = {0};

    // Fallible constructor: an empty path is InvalidPath (no payload).
    assert(kvstore_kv_Store_open(STR(""), &err) == NULL);
    assert(err.code == kvstore_kv_KvError_InvalidPath);
    assert(MSG_EQ(err, "invalid path"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    kvstore_error_clear(&err);
    // A NULL string with a nonzero length is a marshalling failure.
    assert(kvstore_kv_Store_open(NULL, 4, &err) == NULL && err.code == -3);
    kvstore_error_clear(&err);

    kvstore_kv_Store* s = kvstore_kv_Store_new(&err);
    assert(err.code == 0 && s != NULL && path_is(s, "memory"));
    assert(kvstore_kv_Store_capacity(s, &err) == kvstore_kv_Store_default_capacity(&err));
    assert(kvstore_kv_Store_default_capacity(&err) == 1000000);
    kvstore_kv_Store_destroy(s);

    // The async free function completes with a new object...
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_kv_open_store(STR("/async"), on_store, &c);
    wait_for(&c);
    assert(c.code == 0 && c.store != NULL && path_is(c.store, "/async"));
    kvstore_kv_Store_destroy(c.store);
    // ...or with the typed error.
    memset(&c, 0, sizeof c);
    kvstore_kv_open_store(NULL, 0, on_store, &c);
    wait_for(&c);
    assert(c.code == kvstore_kv_KvError_InvalidPath && c.store == NULL);
    assert(strcmp(c.message, "invalid path") == 0);
}

static void basics(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/basics");
    size_t len = 0;

    // put returns the stored entry; the version counts puts of the key.
    kvstore_kv_Entry e;
    assert(try_put(s, "alpha", "one", kvstore_kv_EntryKind_Persistent, NULL, &e, &err));
    assert(str_is(e.key, "alpha") && e.value.len == 3 && memcmp(e.value.ptr, "one", 3) == 0);
    assert(e.kind == kvstore_kv_EntryKind_Persistent && e.version == 1);
    assert(e.expires_at == NULL && e.tags.len == 0 && e.metadata.len == 0);
    kvstore_kv_Entry_free(&e);
    assert(try_put(s, "alpha", "two", kvstore_kv_EntryKind_Volatile, NULL, &e, &err));
    assert(e.version == 2 && e.kind == kvstore_kv_EntryKind_Volatile);
    kvstore_kv_Entry_free(&e);

    // get (throwing record) and find (optional record).
    const uint8_t* buf = kvstore_kv_Store_get(s, STR("alpha"), &len, &err);
    assert(err.code == 0 && buf != NULL);
    assert(kvstore_kv_Entry_decode(buf, len, &e));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(e.value.len == 3 && memcmp(e.value.ptr, "two", 3) == 0);
    kvstore_kv_Entry_free(&e);
    assert(kvstore_kv_Store_get(s, STR("nope"), &len, &err) == NULL);
    assert(MSG_EQ(err, "key not found: nope"));
    expect_key_not_found(&err, "nope");

    buf = kvstore_kv_Store_find(s, STR("alpha"), &len, &err);
    kvstore_kv_Entry* found = NULL;
    assert(err.code == 0 && kvstore_opt_Entry_decode(buf, len, &found));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(found != NULL && found->version == 2);
    kvstore_opt_Entry_free(&found);
    buf = kvstore_kv_Store_find(s, STR("nope"), &len, &err);
    assert(err.code == 0 && kvstore_opt_Entry_decode(buf, len, &found) && found == NULL);
    kvstore_free_bytes((uint8_t*)buf, len);

    // TTLs follow the logical clock; an expired get reports when.
    const int64_t ttl = 10;
    assert(try_put(s, "ttl", "x", kvstore_kv_EntryKind_Volatile, &ttl, &e, &err));
    assert(e.expires_at != NULL && *e.expires_at == 10);
    kvstore_kv_Entry_free(&e);
    assert(kvstore_kv_Store_now(s, &err) == 0);
    assert(kvstore_kv_Store_tick(s, 9, &err) == 9 && count(s) == 2);
    assert(kvstore_kv_Store_tick(s, 1, &err) == 10 && count(s) == 1);
    assert(kvstore_kv_Store_get(s, STR("ttl"), &len, &err) == NULL);
    assert(err.code == kvstore_kv_KvError_Expired);
    kvstore_kv_KvError_Expired_payload expired;
    assert(kvstore_kv_KvError_Expired_payload_decode(err.payload_ptr, err.payload_len, &expired));
    assert(str_is(expired.key, "ttl") && expired.expired_at == 10);
    kvstore_kv_KvError_Expired_payload_free(&expired);
    kvstore_error_clear(&err);
    assert(kvstore_kv_Store_get(s, STR("ttl"), &len, &err) == NULL);
    expect_key_not_found(&err, "ttl");  // the expired read removed it

    // Capacity: a new key past it is StoreFull { capacity }.
    kvstore_kv_Store_set_capacity(s, 1, &err);
    assert(kvstore_kv_Store_capacity(s, &err) == 1);
    put(s, "alpha", "three");  // replacing is fine
    assert(!try_put(s, "beta", "b", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_kv_KvError_StoreFull);
    kvstore_kv_KvError_StoreFull_payload full;
    assert(kvstore_kv_KvError_StoreFull_payload_decode(err.payload_ptr, err.payload_len, &full));
    assert(full.capacity == 1);
    kvstore_error_clear(&err);
    kvstore_kv_Store_set_capacity(s, 100, &err);

    // An undeclared enum value and invalid UTF-8 are marshalling failures.
    assert(!try_put(s, "k", "v", (kvstore_kv_EntryKind)9, NULL, NULL, &err));
    assert(err.code == -3);
    kvstore_error_clear(&err);
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    assert(kvstore_kv_Store_put(s, BYTES(bad_utf8), BYTES(bad_utf8),
                                kvstore_kv_EntryKind_Volatile, false, 0, &len, &err) == NULL);
    assert(err.code == -3);
    kvstore_error_clear(&err);
    // A method on a null receiver too.
    kvstore_kv_Store_count(NULL, &err);
    assert(err.code == -3);
    kvstore_error_clear(&err);

    // delete, clear, and the deprecated size().
    put(s, "beta", "b");
    assert(kvstore_kv_Store_delete(s, STR("beta"), &err) && err.code == 0);
    assert(!kvstore_kv_Store_delete(s, STR("beta"), &err));
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    assert(kvstore_kv_Store_size(s, &err) == 1);
#pragma clang diagnostic pop
    assert(kvstore_kv_Store_clear(s, &err) == 1 && count(s) == 0);
    kvstore_kv_Store_destroy(s);
}

static void iterators(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/iter");
    put(s, "user.bob", "b");
    put(s, "user.alice", "a");
    put(s, "sys.x", "xx");

    // keys(prefix?): owned (ptr, len) strings in key order.
    const char* want_all[3] = {"sys.x", "user.alice", "user.bob"};
    kvstore_writer w = prefix_buf(NULL);
    kvstore_kv_Store_KeysIterator* it = kvstore_kv_Store_keys(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    assert(err.code == 0 && it != NULL);
    const uint8_t* item = NULL;
    size_t len = 0;
    int n = 0;
    while (kvstore_kv_Store_KeysIterator_next(it, &item, &len, &err) == 1) {
        assert(n < 3 && bytes_eq(item, len, want_all[n]));
        kvstore_free_bytes((uint8_t*)item, len);
        n++;
    }
    assert(n == 3 && err.code == 0);
    // Pulling past the end stays at the end.
    assert(kvstore_kv_Store_KeysIterator_next(it, &item, &len, &err) == 0);
    kvstore_kv_Store_KeysIterator_destroy(it);

    // A prefix that matches nothing is KeyNotFound { key: prefix }.
    w = prefix_buf("zzz");
    assert(kvstore_kv_Store_keys(s, w.ptr, w.len, &err) == NULL);
    kvstore_writer_free(&w);
    expect_key_not_found(&err, "zzz");

    // Abandoning an iterator part-way releases it (and its items so far).
    w = prefix_buf("user.");
    it = kvstore_kv_Store_keys(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    assert(kvstore_kv_Store_KeysIterator_next(it, &item, &len, &err) == 1);
    assert(bytes_eq(item, len, "user.alice"));
    kvstore_free_bytes((uint8_t*)item, len);
    kvstore_kv_Store_KeysIterator_destroy(it);

    // entries(prefix?): records.
    w = prefix_buf("sys.");
    kvstore_kv_Store_EntriesIterator* et = kvstore_kv_Store_entries(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    assert(kvstore_kv_Store_EntriesIterator_next(et, &item, &len, &err) == 1);
    kvstore_kv_Entry e;
    assert(kvstore_kv_Entry_decode(item, len, &e));
    kvstore_free_bytes((uint8_t*)item, len);
    assert(str_is(e.key, "sys.x") && e.value.len == 2);
    kvstore_kv_Entry_free(&e);
    assert(kvstore_kv_Store_EntriesIterator_next(et, &item, &len, &err) == 0);
    kvstore_kv_Store_EntriesIterator_destroy(et);

    // partition(prefixes): objects, created as they're pulled.
    kvstore_str prefixes[3] = {kvstore_str_of("user."), kvstore_str_of("sys."),
                               kvstore_str_of("none.")};
    kvstore_list_string pl;
    pl.items = prefixes;
    pl.len = 3;
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &pl);
    kvstore_kv_Store_PartitionIterator* pt = kvstore_kv_Store_partition(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    const uint32_t want_counts[3] = {2, 1, 0};
    kvstore_kv_Store* part = NULL;
    n = 0;
    while (kvstore_kv_Store_PartitionIterator_next(pt, &part, &err) == 1) {
        assert(part != NULL && part != s && count(part) == want_counts[n]);
        char* p = path_of(part);
        assert(bytes_eq((const uint8_t*)p, strlen(p), prefixes[n].ptr));
        free(p);
        kvstore_kv_Store_destroy(part);  // each element is an owned reference
        n++;
    }
    assert(n == 3 && err.code == 0);
    kvstore_kv_Store_PartitionIterator_destroy(pt);
    kvstore_kv_Store_destroy(s);
}

static void listeners(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/listen");
    size_t len = 0;

    listener_ctx* l = new_listener("quiet", NULL);
    uint32_t id = kvstore_kv_Store_subscribe(s, l, &LISTENER_VTABLE, &err);
    assert(err.code == 0 && id > 0);
    assert(kvstore_kv_Store_listener_count(s, &err) == 1);

    put(s, "a", "1");
    assert(atomic_load(&l->puts) == 1 && l->last_version == 1 && !l->last_replaced);
    put(s, "a", "2");
    assert(atomic_load(&l->puts) == 2 && l->last_version == 2 && l->last_replaced);
    assert(strcmp(l->last_key, "a") == 0);
    put(s, "quiet", "x");  // accepts() said no
    assert(atomic_load(&l->puts) == 2);
    assert(kvstore_kv_Store_delete(s, STR("a"), &err));
    assert(atomic_load(&l->removed) == 1 && strcmp(l->last_key, "a") == 0);

    // An expired read removes the entry and says so.
    const int64_t ttl = 1;
    assert(try_put(s, "short", "x", kvstore_kv_EntryKind_Volatile, &ttl, NULL, &err));
    kvstore_kv_Store_tick(s, 1, &err);
    assert(kvstore_kv_Store_get(s, STR("short"), &len, &err) == NULL);
    assert(err.code == kvstore_kv_KvError_Expired);
    kvstore_error_clear(&err);
    assert(atomic_load(&l->expired) == 1 && strcmp(l->last_key, "short") == 0);

    assert(kvstore_kv_Store_clear(s, &err) == 1);  // "quiet" was left
    assert(atomic_load(&l->cleared) == 1 && l->last_cleared == 1);
    assert(atomic_load(&l->off_main_thread) == 0 && "synchronous calls notify inline");

    // Unsubscribing releases the listener once.
    assert(kvstore_kv_Store_unsubscribe(s, id, &err));
    assert(atomic_load(&g_listeners_freed) == 1);
    assert(!kvstore_kv_Store_unsubscribe(s, id, &err));
    assert(kvstore_kv_Store_listener_count(s, &err) == 0);

    // A listener that fails is detached (and released); the put succeeds.
    listener_ctx* failing = new_listener(NULL, "boom");
    kvstore_kv_Store_subscribe(s, failing, &LISTENER_VTABLE, &err);
    put(s, "fine", "1");
    assert(atomic_load(&failing->puts) == 1);
    put(s, "boom", "1");
    assert(err.code == 0 && count(s) == 2);
    assert(kvstore_kv_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 2);

    // A vtable smaller than the producer's is rejected (-3) and released.
    kvstore_kv_Listener_vtable short_vtable = LISTENER_VTABLE;
    short_vtable.size = (uint32_t)(sizeof short_vtable - sizeof short_vtable.on_change);
    kvstore_kv_Store_subscribe(s, new_listener(NULL, NULL), &short_vtable, &err);
    assert(err.code == -3);
    kvstore_error_clear(&err);
    assert(atomic_load(&g_listeners_freed) == 3);

    // A thread-affine listener: compaction notifies from a producer thread,
    // where the producer refuses to call `accepts` (it returns a value), so
    // the listener fails (-4, never called) and is detached. A synchronous
    // put on this thread still reaches it.
    listener_ctx* affine = new_listener(NULL, NULL);
    kvstore_kv_Store_subscribe(s, affine, &AFFINE_LISTENER_VTABLE, &err);
    assert(err.code == 0 && kvstore_kv_Store_listener_count(s, &err) == 1);
    const int64_t one = 1;
    assert(try_put(s, "brief", "x", kvstore_kv_EntryKind_Volatile, &one, NULL, &err));
    assert(atomic_load(&affine->puts) == 1);
    kvstore_kv_Store_tick(s, 1, &err);
    int off_main = atomic_load(&g_listener_calls_off_main);
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_kv_Store_compact(s, 0, NULL, on_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 1);
    // Detached and released without one call off this thread.
    assert(atomic_load(&g_listener_calls_off_main) == off_main);
    assert(kvstore_kv_Store_listener_count(s, &err) == 0);
    assert(atomic_load(&g_listeners_freed) == 4);

    // Destroying the store releases the listeners it still holds.
    kvstore_kv_Store_subscribe(s, new_listener(NULL, NULL), &LISTENER_VTABLE, &err);
    kvstore_kv_Store_subscribe(s, new_listener(NULL, NULL), &LISTENER_VTABLE, &err);
    assert(kvstore_kv_Store_listener_count(s, &err) == 2);
    kvstore_kv_Store_destroy(s);
    assert(atomic_load(&g_listeners_freed) == 6);
}

static void policies(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/policy");
    kvstore_kv_Store* other = open_store("/other");

    policy_ctx* p = (policy_ctx*)calloc(1, sizeof *p);
    p->other = kvstore_kv_Store_clone(other);
    kvstore_kv_Store_set_policy(s, p, &POLICY_VTABLE, &err);
    assert(err.code == 0 && kvstore_kv_Store_has_policy(s, &err));

    // admit's record return is what's stored (its key and version aside).
    kvstore_kv_Entry e;
    assert(try_put(s, "a", "1", kvstore_kv_EntryKind_Volatile, NULL, &e, &err));
    assert(str_is(e.key, "a") && e.version == 1 && e.kind == kvstore_kv_EntryKind_Encrypted);
    assert(e.tags.len == 1 && str_is(e.tags.items[0], "admitted"));
    kvstore_kv_Entry_free(&e);

    // route: the object parameter and object return redirect a write.
    assert(try_put(s, "b/x", "2", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    assert(count(s) == 1 && count(other) == 1);

    // A typed error from the throwing callback reaches the caller with its
    // code and payload, and the domain's own message for them.
    assert(!try_put(s, "secret", "3", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_kv_KvError_Rejected);
    assert(MSG_EQ(err, "write to secret rejected: no secrets"));
    kvstore_kv_KvError_Rejected_payload r;
    assert(kvstore_kv_KvError_Rejected_payload_decode(err.payload_ptr, err.payload_len, &r));
    assert(str_is(r.key, "secret") && str_is(r.reason, "no secrets"));
    kvstore_kv_KvError_Rejected_payload_free(&r);
    kvstore_error_clear(&err);

    // A code outside the domain reaches the producer as a callback failure,
    // which the sample turns into `CallbackFailed` with the consumer's
    // message.
    assert(!try_put(s, "boom", "4", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, "policy exploded");
    // So is a return the runtime can't accept: a malformed record...
    assert(!try_put(s, "garbage", "5", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, NULL);
    // ...or a null required object.
    assert(!try_put(s, "null/x", "6", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    expect_callback_failed(&err, NULL);
    assert(count(s) == 1 && count(other) == 1);
    assert(atomic_load(&p->admitted) == 6);

    // ttl_for: an optional scalar in and out, consulted before admit.
    kvstore_kv_Entry e2;
    const int64_t five = 5;
    assert(try_put(s, "short", "x", kvstore_kv_EntryKind_Volatile, NULL, &e2, &err));
    assert(e2.expires_at != NULL && *e2.expires_at == 1);
    kvstore_kv_Entry_free(&e2);
    assert(try_put(s, "forever", "x", kvstore_kv_EntryKind_Volatile, &five, &e2, &err));
    assert(e2.expires_at == NULL);
    kvstore_kv_Entry_free(&e2);
    assert(try_put(s, "kept", "x", kvstore_kv_EntryKind_Volatile, &five, &e2, &err));
    assert(e2.expires_at != NULL && *e2.expires_at == 5);
    kvstore_kv_Entry_free(&e2);
    assert(!try_put(s, "ttl-fail", "x", kvstore_kv_EntryKind_Volatile, NULL, NULL, &err));
    assert(err.code == kvstore_kv_KvError_InvalidPath && MSG_EQ(err, "invalid path"));
    kvstore_error_clear(&err);
    assert(count(s) == 4 && atomic_load(&p->admitted) == 9);

    // Replacing the policy releases the old one; NULL removes it.
    policy_ctx* p2 = (policy_ctx*)calloc(1, sizeof *p2);
    p2->other = kvstore_kv_Store_clone(other);
    kvstore_kv_Store_set_policy(s, p2, &POLICY_VTABLE, &err);
    assert(atomic_load(&g_policies_freed) == 1);
    kvstore_kv_Store_set_policy(s, NULL, NULL, &err);
    assert(err.code == 0 && atomic_load(&g_policies_freed) == 2);
    assert(!kvstore_kv_Store_has_policy(s, &err));
    put(s, "secret", "now allowed");
    assert(count(s) == 5);

    kvstore_kv_Store_destroy(other);
    kvstore_kv_Store_destroy(s);
}

static void loaders(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/load");
    kvstore_kv_Entry* e = NULL;

    // No loader (a null optional callback): a miss is none.
    assert(get_or_load(s, "k", NULL, &e, &err) && e == NULL);

    // load's bytes are stored, tagged with the loader's name.
    assert(get_or_load(s, "k", new_loader(NULL), &e, &err) && e != NULL);
    assert(e->value.len == 8 && memcmp(e->value.ptr, "loaded:k", 8) == 0);
    assert(e->kind == kvstore_kv_EntryKind_Volatile && e->metadata.len == 1);
    assert(str_is(e->metadata.keys[0], "source") && str_is(e->metadata.values[0], "c-loader"));
    kvstore_opt_Entry_free(&e);
    assert(atomic_load(&g_loaders_freed) == 1 && "a loader is released after the call");
    assert(count(s) == 1);
    // A hit doesn't consult the loader.
    assert(get_or_load(s, "k", new_loader(NULL), &e, &err) && e != NULL && e->version == 1);
    kvstore_opt_Entry_free(&e);

    // The fallback store (an optional object return) is consulted first.
    kvstore_kv_Store* backup = open_store("/backup");
    put(backup, "fb", "from backup");
    assert(get_or_load(s, "fb", new_loader(kvstore_kv_Store_clone(backup)), &e, &err));
    assert(e != NULL && e->value.len == 11 && e->kind == kvstore_kv_EntryKind_Persistent);
    kvstore_opt_Entry_free(&e);
    kvstore_kv_Store_destroy(backup);

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
    kvstore_kv_Store_destroy(s);
}

static void async_calls(void) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = open_store("/async-calls");
    listener_ctx* l = new_listener(NULL, NULL);
    kvstore_kv_Store_subscribe(s, l, &LISTENER_VTABLE, &err);

    const int64_t ttl = 1;
    assert(try_put(s, "old1", "x", kvstore_kv_EntryKind_Volatile, &ttl, NULL, &err));
    assert(try_put(s, "old2", "x", kvstore_kv_EntryKind_Volatile, &ttl, NULL, &err));
    put(s, "keep", "x");
    kvstore_kv_Store_tick(s, 5, &err);

    // compact runs on a producer thread and notifies listeners there.
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_cancel_token* token = kvstore_cancel_token_create();
    kvstore_kv_Store_compact(s, 0, token, on_u32, &c);
    kvstore_cancel_token_destroy(token);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 2);
    assert(atomic_load(&l->expired) == 2);
    assert(atomic_load(&l->off_main_thread) == 2 && "notified from a producer thread");
    assert(count(s) == 1);

    // A null token never cancels.
    memset(&c, 0, sizeof c);
    kvstore_kv_Store_compact(s, 5, NULL, on_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 0);

    // Cancel mid-pause: the call completes with -5 at once, and the
    // background pause notices the token and stops.
    memset(&c, 0, sizeof c);
    token = kvstore_cancel_token_create();
    kvstore_kv_Store_compact(s, 60000, token, on_u32, &c);
    sleep_ms(20);
    assert(!atomic_load(&c.done));
    assert(kvstore_kv_Store_active_jobs(&err) >= 1);
    kvstore_cancel_token_cancel(token);
    kvstore_cancel_token_destroy(token);
    wait_for(&c);
    assert(c.code == -5);
    int stopped = 0;
    for (int i = 0; i < 2000 && !stopped; i++) {
        stopped = kvstore_kv_Store_active_jobs(&err) == 0;
        if (!stopped) sleep_ms(1);
    }
    assert(stopped && "the cancelled pause stopped cooperatively");

    // get_many: an async list of optional records, launched concurrently.
    kvstore_str keys[3] = {kvstore_str_of("keep"), kvstore_str_of("gone"),
                           kvstore_str_of("keep")};
    kvstore_list_string kl;
    kl.items = keys;
    kl.len = 3;
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &kl);
    enum { CONCURRENT = 32 };
    static call_state many[CONCURRENT];
    memset(many, 0, sizeof many);
    for (int i = 0; i < CONCURRENT; i++) {
        kvstore_kv_Store_get_many(s, w.ptr, w.len, on_buffer, &many[i]);
    }
    kvstore_writer_free(&w);
    for (int i = 0; i < CONCURRENT; i++) {
        wait_for(&many[i]);
        assert(many[i].code == 0);
        kvstore_list_opt_Entry got;
        assert(kvstore_list_opt_Entry_decode(many[i].buf, many[i].len, &got));
        assert(got.len == 3 && got.items[0] != NULL && got.items[1] == NULL);
        assert(got.items[2] != NULL && str_is(got.items[2]->key, "keep"));
        kvstore_list_opt_Entry_free(&got);
        free(many[i].buf);
    }

    // The nested module's async free function: objects in a list in,
    // a record out.
    kvstore_kv_Store* other = open_store("/other");
    put(other, "a", "123");
    kvstore_kv_Store* both[2] = {s, other};
    kvstore_list_Store sl;
    sl.items = both;
    sl.len = 2;
    memset(&w, 0, sizeof w);
    kvstore_list_Store_write(&w, &sl);
    memset(&c, 0, sizeof c);
    kvstore_kv_stats_summarize_all(w.ptr, w.len, on_buffer, &c);
    kvstore_writer_free(&w);
    wait_for(&c);
    assert(c.code == 0);
    kvstore_kv_stats_Stats st;
    assert(kvstore_kv_stats_Stats_decode(c.buf, c.len, &st));
    free(c.buf);
    assert(st.entries == 2 && st.bytes == 4);
    assert(st.by_kind.len == 1 && st.by_kind.keys[0] == kvstore_kv_EntryKind_Persistent);
    assert(st.by_kind.values[0] == 2);
    kvstore_kv_stats_Stats_free(&st);

    kvstore_kv_Store_destroy(other);
    kvstore_kv_Store_destroy(s);
}

static kvstore_kv_StoreInfo describe(kvstore_kv_Store* s, const char* label,
                                     const kvstore_kv_Store* mirror) {
    kvstore_error err = {0};
    size_t len = 0;
    const uint8_t* buf = kvstore_kv_Store_describe(s, STR(label), mirror, &len, &err);
    assert(err.code == 0 && buf != NULL);
    kvstore_kv_StoreInfo info;
    assert(kvstore_kv_StoreInfo_decode(buf, len, &info));
    kvstore_free_bytes((uint8_t*)buf, len);
    return info;
}

static void object_graph(void) {
    kvstore_error err = {0};
    size_t len = 0;
    kvstore_kv_Store* s = open_store("/graph");
    put(s, "k", "v");

    // share(): the same object; clone/destroy count references.
    kvstore_kv_Store* shared = kvstore_kv_Store_share(s, &err);
    assert(err.code == 0 && shared == s);
    kvstore_kv_Store_destroy(s);
    assert(count(shared) == 1 && "alive through the shared reference");
    s = shared;
    kvstore_kv_Store* cloned = kvstore_kv_Store_clone(s);
    assert(cloned == s);
    kvstore_kv_Store_destroy(cloned);
    assert(kvstore_kv_Store_clone(NULL) == NULL);
    kvstore_kv_Store_destroy(NULL);

    // fork(): a distinct object with a copy of the entries.
    kvstore_kv_Store* fork = kvstore_kv_Store_fork(s, &err);
    assert(fork != NULL && fork != s && count(fork) == 1 && path_is(fork, "/graph"));
    put(fork, "k2", "v");
    assert(count(fork) == 2 && count(s) == 1);

    // larger(): `Store?` in and out.
    kvstore_kv_Store* empty = open_store("/empty");
    assert(kvstore_kv_Store_larger(empty, NULL, &err) == NULL && err.code == 0);
    kvstore_kv_Store* bigger = kvstore_kv_Store_larger(empty, fork, &err);
    assert(bigger == fork);
    kvstore_kv_Store_destroy(bigger);
    bigger = kvstore_kv_Store_larger(s, NULL, &err);
    assert(bigger == s);
    kvstore_kv_Store_destroy(bigger);

    // describe(): a record whose fields carry objects.
    kvstore_kv_StoreInfo info = describe(s, "main", fork);
    assert(str_is(info.label, "main") && info.store == s && info.mirror == fork);
    assert(info.count == 1 && count(info.mirror) == 2);

    // open_many(): a list of objects; one bad path fails the whole call.
    kvstore_str paths[2] = {kvstore_str_of("/a"), kvstore_str_of("/b")};
    kvstore_list_string pl;
    pl.items = paths;
    pl.len = 2;
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &pl);
    const uint8_t* buf = kvstore_kv_Store_open_many(w.ptr, w.len, &len, &err);
    kvstore_writer_free(&w);
    kvstore_list_Store many;
    assert(err.code == 0 && kvstore_list_Store_decode(buf, len, &many));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(many.len == 2 && path_is(many.items[0], "/a") && path_is(many.items[1], "/b"));
    paths[1] = kvstore_str_of("");
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &pl);
    assert(kvstore_kv_Store_open_many(w.ptr, w.len, &len, &err) == NULL);
    kvstore_writer_free(&w);
    assert(err.code == kvstore_kv_KvError_InvalidPath);
    kvstore_error_clear(&err);

    // by_label(): records with objects in, a map with object values out.
    kvstore_kv_StoreInfo infos[2];
    infos[0] = info;
    infos[1].label = kvstore_str_of("first");
    infos[1].store = many.items[0];
    infos[1].mirror = NULL;
    infos[1].count = 0;
    kvstore_list_StoreInfo il;
    il.items = infos;
    il.len = 2;
    memset(&w, 0, sizeof w);
    kvstore_list_StoreInfo_write(&w, &il);
    buf = kvstore_kv_Store_by_label(w.ptr, w.len, &len, &err);
    kvstore_writer_free(&w);
    kvstore_map_string_Store named;
    assert(err.code == 0 && kvstore_map_string_Store_decode(buf, len, &named));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(named.len == 2);
    for (size_t i = 0; i < named.len; i++) {
        if (str_is(named.keys[i], "main")) assert(named.values[i] == s);
        else assert(str_is(named.keys[i], "first") && named.values[i] == many.items[0]);
    }

    // total_count(): a list, a map, and an optional record, all carrying
    // objects (each written as a fresh reference the producer adopts).
    put(many.items[0], "m", "1");
    kvstore_kv_Store* three[3] = {many.items[0], many.items[1], fork};
    kvstore_list_Store stores;
    stores.items = three;
    stores.len = 3;
    kvstore_writer ws, wn, we;
    memset(&ws, 0, sizeof ws);
    memset(&wn, 0, sizeof wn);
    memset(&we, 0, sizeof we);
    kvstore_list_Store_write(&ws, &stores);
    kvstore_map_string_Store_write(&wn, &named);
    kvstore_opt_StoreInfo_write(&we, &info);
    // stores: 1 + 0 + 2; named: main 1 + first 1; extra: main 1.
    assert(kvstore_kv_Store_total_count(ws.ptr, ws.len, wn.ptr, wn.len, we.ptr, we.len, &err) ==
           6);
    assert(err.code == 0);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&wn);
    kvstore_writer_free(&we);
    // A buffer carrying objects is consumed by the call that receives it
    // (its references are adopted), so each call gets fresh buffers.
    memset(&ws, 0, sizeof ws);
    memset(&wn, 0, sizeof wn);
    memset(&we, 0, sizeof we);
    kvstore_list_Store_write(&ws, &stores);
    kvstore_map_string_Store_write(&wn, &named);
    kvstore_opt_StoreInfo_write(&we, NULL);
    assert(kvstore_kv_Store_total_count(ws.ptr, ws.len, wn.ptr, wn.len, we.ptr, we.len, &err) ==
           5);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&wn);
    kvstore_writer_free(&we);

    // Everything we hold is still intact; release each reference once.
    assert(count(s) == 1 && count(fork) == 2 && count(many.items[0]) == 1);
    kvstore_map_string_Store_free(&named);
    kvstore_kv_StoreInfo_free(&info);
    kvstore_list_Store_free(&many);
    kvstore_kv_Store_destroy(empty);
    kvstore_kv_Store_destroy(fork);
    kvstore_kv_Store_destroy(s);
}

static void stats_and_report(void) {
    kvstore_error err = {0};
    size_t len = 0;
    kvstore_kv_Store* s = open_store("/stats");
    put(s, "b", "12");
    put(s, "a", "1");
    put(s, "a", "123");
    assert(try_put(s, "c", "x", kvstore_kv_EntryKind_Encrypted, NULL, NULL, &err));

    // kv.stats.summarize: the parent's Store as a parameter, the parent's
    // error domain for a prefix that matches nothing.
    kvstore_writer w = prefix_buf(NULL);
    const uint8_t* buf = kvstore_kv_stats_summarize(s, w.ptr, w.len, &len, &err);
    kvstore_writer_free(&w);
    kvstore_kv_stats_Stats st;
    assert(err.code == 0 && kvstore_kv_stats_Stats_decode(buf, len, &st));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(st.entries == 3 && st.bytes == 6 && st.by_kind.len == 2);
    for (size_t i = 0; i < st.by_kind.len; i++) {
        if (st.by_kind.keys[i] == kvstore_kv_EntryKind_Persistent) assert(st.by_kind.values[i] == 2);
        else assert(st.by_kind.keys[i] == kvstore_kv_EntryKind_Encrypted && st.by_kind.values[i] == 1);
    }
    kvstore_kv_stats_Stats_free(&st);
    w = prefix_buf("q");
    assert(kvstore_kv_stats_summarize(s, w.ptr, w.len, &len, &err) == NULL);
    kvstore_writer_free(&w);
    expect_key_not_found(&err, "q");

    // report.render_report: the sibling root shares the Entry record.
    w = prefix_buf(NULL);
    kvstore_kv_Store_EntriesIterator* it = kvstore_kv_Store_entries(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    kvstore_kv_Entry entries[3];
    const uint8_t* item = NULL;
    size_t n = 0;
    while (kvstore_kv_Store_EntriesIterator_next(it, &item, &len, &err) == 1) {
        assert(kvstore_kv_Entry_decode(item, len, &entries[n++]));
        kvstore_free_bytes((uint8_t*)item, len);
    }
    kvstore_kv_Store_EntriesIterator_destroy(it);
    assert(n == 3);
    kvstore_list_Entry el;
    el.items = entries;
    el.len = n;
    memset(&w, 0, sizeof w);
    kvstore_list_Entry_write(&w, &el);
    buf = kvstore_report_render_report(w.ptr, w.len, &len, &err);
    kvstore_writer_free(&w);
    kvstore_list_string lines;
    assert(err.code == 0 && kvstore_list_string_decode(buf, len, &lines));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(lines.len == 3);
    assert(str_is(lines.items[0], "a: 3 bytes, Persistent, v2"));
    assert(str_is(lines.items[1], "b: 2 bytes, Persistent"));
    assert(str_is(lines.items[2], "c: 1 bytes, Encrypted"));
    kvstore_list_string_free(&lines);
    for (size_t i = 0; i < n; i++) kvstore_kv_Entry_free(&entries[i]);

    el.len = 0;
    memset(&w, 0, sizeof w);
    kvstore_list_Entry_write(&w, &el);
    assert(kvstore_report_render_report(w.ptr, w.len, &len, &err) == NULL);
    kvstore_writer_free(&w);
    assert(err.code == kvstore_report_ReportError_NothingToReport);
    assert(MSG_EQ(err, "nothing to report"));
    kvstore_error_clear(&err);
    kvstore_kv_Store_destroy(s);
}

// ── scorer (consumer-implemented, typed arrays in and out) ─────────────────

typedef enum { SCORE_SIZES, SCORE_FAIL, SCORE_SHORT } score_mode;

typedef struct {
    score_mode mode;
} scorer_ctx;

static atomic_int g_scorers_freed;

// `sizes` is a borrowed typed array; the scores are a run of `count *
// sizeof(double)` bytes from kvstore_alloc, which the producer adopts.
static void scorer_scores(void* ctx, const uint64_t* sizes_ptr, size_t sizes_len,
                          double** out_ptr, size_t* out_len, kvstore_error* out_err) {
    scorer_ctx* sc = (scorer_ctx*)ctx;
    assert(((uintptr_t)sizes_ptr % 8) == 0 && "a borrowed typed array is aligned");
    if (sc->mode == SCORE_SIZES) {
        // The store's value sizes in key order: a="1", b="333", c="22".
        assert(sizes_len == 3 && sizes_ptr[0] == 1 && sizes_ptr[1] == 3 && sizes_ptr[2] == 2);
    }
    if (sc->mode == SCORE_FAIL) {
        kvstore_error_set(out_err, -1, STR("scorer is out of order"));
        return;
    }
    size_t n = sc->mode == SCORE_SHORT ? 1 : sizes_len;
    double* scores = (double*)(void*)kvstore_alloc(n * sizeof(double));
    for (size_t i = 0; i < n; i++) scores[i] = (double)sizes_ptr[i];
    *out_ptr = scores;
    *out_len = n;
}

static void scorer_free(void* ctx) {
    free(ctx);
    atomic_fetch_add(&g_scorers_freed, 1);
}

static const kvstore_kv_Scorer_vtable SCORER_VTABLE = {
    sizeof(kvstore_kv_Scorer_vtable), 0, scorer_free, scorer_scores,
};

static void on_opt_u32(void* context, kvstore_error* err, bool has_result, uint32_t result) {
    call_state* c = (call_state*)context;
    settle(c, err);
    c->u32 = has_result ? result : UINT32_MAX;
    atomic_store(&c->done, 1);
}

// A typed-array async result is an owned run of `count * sizeof(T)` bytes.
static void on_u32s(void* context, kvstore_error* err, const uint32_t* ptr, size_t len) {
    call_state* c = (call_state*)context;
    settle(c, err);
    if (ptr != NULL) {
        c->buf = (uint8_t*)malloc(len * sizeof(uint32_t));
        memcpy(c->buf, ptr, len * sizeof(uint32_t));
        c->len = len;
        kvstore_free_bytes((uint8_t*)ptr, len * sizeof(uint32_t));
    }
    atomic_store(&c->done, 1);
}

static void abi5_shapes(void) {
    kvstore_error err = {0};
    size_t len = 0;
    kvstore_kv_Store* s = open_store("/abi5");

    const int64_t seven = 7;
    assert(try_put(s, "b", "12", kvstore_kv_EntryKind_Persistent, NULL, NULL, &err));
    assert(try_put(s, "a", "\x01\x02\x03", kvstore_kv_EntryKind_Volatile, &seven, NULL, &err));
    assert(count(s) == 2);

    // An optional scalar return.
    int64_t at = -1;
    assert(kvstore_kv_Store_expires_at(s, STR("a"), &at, &err) && at == 7 && err.code == 0);
    assert(!kvstore_kv_Store_expires_at(s, STR("b"), &at, &err) && err.code == 0);
    assert(!kvstore_kv_Store_expires_at(s, STR("zzz"), &at, &err) && err.code == 0);

    // A typed-array return, in key order.
    uint64_t* sizes = kvstore_kv_Store_value_sizes(s, &len, &err);
    assert(err.code == 0 && len == 2 && sizes[0] == 3 && sizes[1] == 2);
    kvstore_free_bytes((uint8_t*)sizes, len * sizeof(uint64_t));

    // Optional scalar iterator items.
    kvstore_kv_Store_ExpirationsIterator* it = kvstore_kv_Store_expirations(s, &err);
    assert(err.code == 0 && it != NULL);
    bool has = false;
    int64_t item = 0;
    assert(kvstore_kv_Store_ExpirationsIterator_next(it, &has, &item, &err) == 1);
    assert(has && item == 7);
    assert(kvstore_kv_Store_ExpirationsIterator_next(it, &has, &item, &err) == 1);
    assert(!has && item == 0);
    assert(kvstore_kv_Store_ExpirationsIterator_next(it, &has, &item, &err) == 0);
    kvstore_kv_Store_ExpirationsIterator_destroy(it);

    // An optional scalar async result.
    call_state c;
    memset(&c, 0, sizeof c);
    kvstore_kv_Store_version_of(s, STR("a"), on_opt_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == 1);
    memset(&c, 0, sizeof c);
    kvstore_kv_Store_version_of(s, STR("q"), on_opt_u32, &c);
    wait_for(&c);
    assert(c.code == 0 && c.u32 == UINT32_MAX);

    // A typed-array async result.
    put(s, "b", "x");
    kvstore_str keys[3] = {kvstore_str_of("b"), kvstore_str_of("q"), kvstore_str_of("a")};
    kvstore_list_string kl = {keys, 3};
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &kl);
    memset(&c, 0, sizeof c);
    kvstore_kv_Store_versions(s, w.ptr, w.len, on_u32s, &c);
    kvstore_writer_free(&w);
    wait_for(&c);
    assert(c.code == 0 && c.len == 3);
    const uint32_t* versions = (const uint32_t*)(const void*)c.buf;
    assert(versions[0] == 2 && versions[1] == 0 && versions[2] == 1);
    free(c.buf);
    kvstore_kv_Store_destroy(s);

    // A callback taking and returning typed arrays.
    s = open_store("/rank");
    put(s, "a", "1");
    put(s, "b", "333");
    put(s, "c", "22");
    kvstore_list_string ranked;
    scorer_ctx* sc = (scorer_ctx*)calloc(1, sizeof *sc);
    sc->mode = SCORE_SIZES;
    const uint8_t* buf = kvstore_kv_Store_rank(s, sc, &SCORER_VTABLE, &len, &err);
    assert(err.code == 0 && kvstore_list_string_decode(buf, len, &ranked));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(ranked.len == 3 && str_is(ranked.items[0], "b") && str_is(ranked.items[1], "c"));
    assert(str_is(ranked.items[2], "a"));
    kvstore_list_string_free(&ranked);
    sc = (scorer_ctx*)calloc(1, sizeof *sc);
    sc->mode = SCORE_FAIL;
    assert(kvstore_kv_Store_rank(s, sc, &SCORER_VTABLE, &len, &err) == NULL);
    expect_callback_failed(&err, "scorer is out of order");
    sc = (scorer_ctx*)calloc(1, sizeof *sc);
    sc->mode = SCORE_SHORT;
    assert(kvstore_kv_Store_rank(s, sc, &SCORER_VTABLE, &len, &err) == NULL);
    expect_callback_failed(&err, "expected 3 scores, got 1");
    kvstore_kv_Store_destroy(s);

    // `throws: any` and a `usize` return: code -1 and a message, no payload.
    s = open_store("/import");
    assert(kvstore_kv_Store_import_lines(s, STR("a=1\n\nb=two\n"), &err) == 2 && err.code == 0);
    buf = kvstore_kv_Store_get(s, STR("b"), &len, &err);
    kvstore_kv_Entry e;
    assert(err.code == 0 && kvstore_kv_Entry_decode(buf, len, &e));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(e.value.len == 3 && memcmp(e.value.ptr, "two", 3) == 0);
    kvstore_kv_Entry_free(&e);
    assert(kvstore_kv_Store_import_lines(s, STR("c=3\nbroken\nd=4"), &err) == 0);
    assert(err.code == -1 && MSG_EQ(err, "line 2: expected key=value"));
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    kvstore_error_clear(&err);
    assert(count(s) == 3);
    kvstore_kv_Store_destroy(s);
}

int main(void) {
    g_main_thread = pthread_self();

    assert(KVSTORE_ABI_VERSION == 5u && kvstore_abi_version() == KVSTORE_ABI_VERSION);
    assert(kvstore_kv_contract_check() == 0);
    assert(kvstore_report_contract_check() == 0);
    assert(kvstore_debug_live(-1) == 1 && "the sample counts live allocations");

    constructors();
    basics();
    iterators();
    listeners();
    policies();
    loaders();
    async_calls();
    object_graph();
    stats_and_report();
    abi5_shapes();

    ASSERT_NO_LEAKS(kvstore_debug_live);
    assert(atomic_load(&g_listeners_freed) == 7);
    assert(atomic_load(&g_policies_freed) == 2);
    assert(atomic_load(&g_scorers_freed) == 3);
    printf("c/kvstore: OK\n");
    return 0;
}
