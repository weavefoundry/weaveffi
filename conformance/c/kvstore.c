// Conformance consumer: kvstore sample, C target (ABI revision 2).
//
// Exercises the Store interface ABI end to end: the fallible constructor
// (`Store_open`), instance methods taking the receiver as the leading
// argument, the static method (`Store_default_capacity`), the typed
// error-domain codes surfaced through the error-out slot, the iterator
// out-param `next` convention, the `Entry` record decoded from a value
// buffer (list and map fields included), the `kv.stats` submodule, the
// hand-written `EvictionListener` vtable (set -> fire synchronously on
// delete and on expiry-on-read -> replace/clear/detach -> `free`), a
// foreign error raised from the listener surfacing as code -4, the
// reference-counted object graph (`share`, `fork`, `clone`/`destroy`,
// `larger` with `Store?` both ways, `describe` returning a record that
// carries object tokens, `open_many` returning a list of objects,
// `total_count` taking objects inside a list and a record), and the raw
// `_async` launcher with a cancel token. Completion arrives on the
// producer's worker thread (synchronized here with C11 atomics). Exits 0 on
// success; aborts otherwise.

#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "weaveffi.h"
#include "wvbuf.h"

// ── eviction listener (consumer-implemented callback interface) ────────────

typedef struct {
    int evictions;
    char last_key[64];
    int32_t last_reason;
    size_t last_value_len;
    int detach_after;  // return false (detach) once this many evictions ran
    int fail;          // report a foreign error instead of observing
} listener_ctx;

static int g_listener_freed = 0;

// Entry record: id, key, value (bytes), created_at, expires_at?, tags,
// metadata. The buffer is borrowed for the duration of the dispatch.
static bool on_evict(void* ctx, const uint8_t* entry_ptr, size_t entry_len,
                     weaveffi_kv_EvictionReason reason, weaveffi_error* out_err) {
    listener_ctx* l = (listener_ctx*)ctx;
    if (l->fail) {
        weaveffi_error_set(out_err, -4, "listener exploded");
        return true;
    }
    wv_reader r;
    wv_r_init(&r, entry_ptr, entry_len);
    int64_t id = wv_get_i64(&r);
    assert(id > 0);
    char* key = wv_get_str(&r);
    snprintf(l->last_key, sizeof l->last_key, "%s", key);
    free(key);
    l->last_value_len = wv_get_u32(&r);
    wv_take(&r, l->last_value_len);
    assert(wv_get_i64(&r) > 0 && "created_at");
    if (wv_get_bool(&r)) wv_get_i64(&r);  // expires_at?
    uint32_t tags = wv_get_u32(&r);
    for (uint32_t i = 0; i < tags; i++) free(wv_get_str(&r));
    uint32_t meta = wv_get_u32(&r);
    for (uint32_t i = 0; i < meta; i++) {
        free(wv_get_str(&r));
        free(wv_get_str(&r));
    }
    wv_r_expect_end(&r);
    l->last_reason = (int32_t)reason;
    l->evictions++;
    return l->detach_after == 0 || l->evictions < l->detach_after;
}

static void listener_free(void* ctx) {
    free(ctx);
    g_listener_freed++;
}

static const weaveffi_kv_EvictionListener_vtable LISTENER_VTABLE = {
    on_evict,
    listener_free,
};

static listener_ctx* new_listener(int detach_after, int fail) {
    listener_ctx* l = (listener_ctx*)calloc(1, sizeof *l);
    assert(l != NULL);
    l->detach_after = detach_after;
    l->fail = fail;
    return l;
}

// ── async completion state ─────────────────────────────────────────────────
static atomic_int g_compact_done = 0;
static int64_t g_compact_result = -1;
static int32_t g_compact_err = -1;

static void on_compact_done(void* context, weaveffi_error* err, int64_t result) {
    (void)context;
    g_compact_err = err ? err->code : 0;
    weaveffi_error_free(err);
    g_compact_result = result;
    atomic_store(&g_compact_done, 1);
}

static void wait_compact(void) {
    for (int i = 0; i < 5000 && !atomic_load(&g_compact_done); i++) usleep(1000);
    assert(atomic_load(&g_compact_done));
    atomic_store(&g_compact_done, 0);
}

// ── helpers ────────────────────────────────────────────────────────────────

// Optional-i64 parameter encodings for `ttl_seconds`.
static const uint8_t TTL_NONE[1] = {0};

static weaveffi_kv_Store* open_store(const char* path) {
    weaveffi_error err = {0};
    weaveffi_kv_Store* s = weaveffi_kv_Store_open(path, &err);
    assert(err.code == 0 && s != NULL);
    return s;
}

static void put(weaveffi_kv_Store* s, const char* key, const uint8_t* v, size_t n) {
    weaveffi_error err = {0};
    assert(weaveffi_kv_Store_put(s, key, v, n, weaveffi_kv_EntryKind_Persistent,
                                 TTL_NONE, sizeof TTL_NONE, &err));
    assert(err.code == 0);
}

// put with `Some(ttl)`: flag byte + little-endian i64.
static void put_ttl(weaveffi_kv_Store* s, const char* key, const uint8_t* v,
                    size_t n, int64_t ttl) {
    weaveffi_error err = {0};
    wv_writer w;
    wv_w_init(&w);
    wv_put_bool(&w, 1);
    wv_put_i64(&w, ttl);
    assert(weaveffi_kv_Store_put(s, key, v, n, weaveffi_kv_EntryKind_Volatile,
                                 w.buf, w.len, &err));
    assert(err.code == 0);
    wv_w_free(&w);
}

static int64_t count(weaveffi_kv_Store* s) {
    weaveffi_error err = {0};
    int64_t n = weaveffi_kv_Store_count(s, &err);
    assert(err.code == 0);
    return n;
}

// A decoded StoreInfo record: label, store (object token), mirror (object
// token?), count. The adopted references are the caller's to destroy.
typedef struct {
    char* label;
    weaveffi_kv_Store* store;
    weaveffi_kv_Store* mirror;  // NULL when absent
    int64_t count;
} store_info_t;

static void read_store_info(wv_reader* r, store_info_t* info) {
    info->label = wv_get_str(r);
    info->store = (weaveffi_kv_Store*)wv_get_obj(r);
    info->mirror = wv_get_bool(r) ? (weaveffi_kv_Store*)wv_get_obj(r) : NULL;
    info->count = wv_get_i64(r);
}

// Encode a StoreInfo. Object fields are written as fresh strong references
// (`_clone`), since the buffer's reader adopts them.
static void write_store_info(wv_writer* w, const char* label,
                             weaveffi_kv_Store* store, weaveffi_kv_Store* mirror,
                             int64_t n) {
    wv_put_str(w, label);
    wv_put_obj(w, weaveffi_kv_Store_clone(store));
    wv_put_bool(w, mirror != NULL);
    if (mirror) wv_put_obj(w, weaveffi_kv_Store_clone(mirror));
    wv_put_i64(w, n);
}

int main(void) {
    weaveffi_error err = {0};
    const uint8_t payload[3] = {1, 2, 3};

    assert(weaveffi_abi_version() == 2u && WEAVEFFI_ABI_VERSION == 2u);

    // Static method: no receiver, plain error-out slot.
    assert(weaveffi_kv_Store_default_capacity(&err) == 1000000);
    assert(err.code == 0);

    // Fallible constructor, typed error path: an empty path reports the
    // KvError.IoError domain code (1004) with its doc-comment message.
    weaveffi_kv_Store* bad = weaveffi_kv_Store_open("", &err);
    assert(bad == NULL);
    assert(err.code == weaveffi_kv_KvError_IoError);
    assert(err.message != NULL && strcmp(err.message, "I/O failure") == 0);
    weaveffi_error_clear(&err);

    // A null `string` argument is a marshalling failure (-3).
    assert(weaveffi_kv_Store_open(NULL, &err) == NULL);
    assert(err.code == -3);
    weaveffi_error_clear(&err);

    weaveffi_kv_Store* store = open_store("/tmp/conformance-kvstore-c");

    // Populate two keys so count/iterator/stats have something to report.
    put(store, "alpha", payload, sizeof payload);
    assert(weaveffi_kv_Store_put(store, "beta", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Volatile,
                                 TTL_NONE, sizeof TTL_NONE, &err));
    assert(err.code == 0);
    assert(count(store) == 2);

    // An out-of-range enum discriminant is a marshalling failure (-3).
    assert(!weaveffi_kv_Store_put(store, "bad", payload, sizeof payload,
                                  (weaveffi_kv_EntryKind)999, TTL_NONE,
                                  sizeof TTL_NONE, &err));
    assert(err.code == -3);
    weaveffi_error_clear(&err);
    assert(count(store) == 2);

    // Typed error path on a method: a missing key reports KeyNotFound (1001).
    size_t out_len = 0;
    const uint8_t* nope = weaveffi_kv_Store_get(store, "missing", &out_len, &err);
    assert(nope == NULL);
    assert(err.code == weaveffi_kv_KvError_KeyNotFound);
    assert(strcmp(err.message, "key not found") == 0);
    weaveffi_error_clear(&err);

    // Deprecated method still works at the ABI level. The generated header
    // marks it deprecated (that attribute is part of what this consumer
    // verifies), so silence the warning for this one deliberate call.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    assert(weaveffi_kv_Store_legacy_put(store, "old", payload, sizeof payload, &err));
#pragma clang diagnostic pop
    assert(err.code == 0);
    assert(weaveffi_kv_Store_delete(store, "old", &err) && err.code == 0);
    assert(!weaveffi_kv_Store_delete(store, "old", &err) && err.code == 0);

    // Iterator: `next` writes one element per call and returns 1/0. Keys
    // come back sorted. The optional prefix parameter is a buffered
    // `string?`; absent first, then present.
    weaveffi_kv_Store_ListKeysIterator* it =
        weaveffi_kv_Store_list_keys(store, TTL_NONE, sizeof TTL_NONE, &err);
    assert(err.code == 0 && it != NULL);
    const char* expected_keys[] = {"alpha", "beta"};
    int n = 0;
    const char* item = NULL;
    weaveffi_error iter_err = {0};
    while (weaveffi_kv_Store_ListKeysIterator_next(it, &item, &iter_err) != 0) {
        assert(n < 2 && strcmp(item, expected_keys[n]) == 0);
        weaveffi_free_string(item);
        n++;
    }
    assert(iter_err.code == 0);
    weaveffi_kv_Store_ListKeysIterator_destroy(it);
    assert(n == 2);

    wv_writer prefix;
    wv_w_init(&prefix);
    wv_put_bool(&prefix, 1);
    wv_put_str(&prefix, "be");
    it = weaveffi_kv_Store_list_keys(store, prefix.buf, prefix.len, &err);
    assert(err.code == 0 && it != NULL);
    assert(weaveffi_kv_Store_ListKeysIterator_next(it, &item, &iter_err) == 1);
    assert(strcmp(item, "beta") == 0);
    weaveffi_free_string(item);
    assert(weaveffi_kv_Store_ListKeysIterator_next(it, &item, &iter_err) == 0);
    weaveffi_kv_Store_ListKeysIterator_destroy(it);
    wv_w_free(&prefix);

    // get -> buffered `Entry?`: flag byte, then the record's fields in
    // declaration order (id, key, value, created_at, expires_at?, tags,
    // metadata). The list and map fields nest inside the same buffer.
    const uint8_t* ebuf = weaveffi_kv_Store_get(store, "alpha", &out_len, &err);
    assert(err.code == 0 && ebuf != NULL);
    wv_reader r;
    wv_r_init(&r, ebuf, out_len);
    assert(wv_get_bool(&r) == 1 && "entry present");
    int64_t id = wv_get_i64(&r);
    assert(id == 1 && "first id handed out by this store");
    char* key = wv_get_str(&r);
    assert(strcmp(key, "alpha") == 0);
    free(key);
    uint32_t vlen = wv_get_u32(&r);
    assert(vlen == 3);
    const uint8_t* vbytes = wv_take(&r, vlen);
    assert(memcmp(vbytes, payload, 3) == 0);
    int64_t created_at = wv_get_i64(&r);
    assert(created_at > 0);
    assert(wv_get_bool(&r) == 0 && "no TTL: expires_at is absent");
    assert(wv_get_u32(&r) == 0 && "no tags");
    assert(wv_get_u32(&r) == 0 && "no metadata");
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ebuf, out_len);

    // kv.stats submodule: the Stats record is three i64 fields in one buffer.
    const uint8_t* sbuf = weaveffi_kv_stats_get_stats(store, &out_len, &err);
    assert(err.code == 0 && sbuf != NULL);
    wv_r_init(&r, sbuf, out_len);
    assert(wv_get_i64(&r) == 2 && "total_entries");
    assert(wv_get_i64(&r) == 6 && "total_bytes: two 3-byte values");
    assert(wv_get_i64(&r) == 0 && "expired_entries");
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)sbuf, out_len);

    // ── eviction listener ────────────────────────────────────────────────
    // delete fires on_evict synchronously on the calling thread with the
    // decoded Entry and reason Deleted.
    listener_ctx* l1 = new_listener(0, 0);
    weaveffi_kv_Store_set_eviction_listener(store, l1, &LISTENER_VTABLE, &err);
    assert(err.code == 0);
    assert(weaveffi_kv_Store_delete(store, "beta", &err) && err.code == 0);
    assert(l1->evictions == 1);
    assert(strcmp(l1->last_key, "beta") == 0);
    assert(l1->last_reason == weaveffi_kv_EvictionReason_Deleted);
    assert(l1->last_value_len == 3);

    // An expired entry is evicted on read: get reports Expired (1002) and
    // the listener sees reason Expired.
    put_ttl(store, "stale", payload, 2, -1);
    assert(weaveffi_kv_Store_get(store, "stale", &out_len, &err) == NULL);
    assert(err.code == weaveffi_kv_KvError_Expired);
    weaveffi_error_clear(&err);
    assert(l1->evictions == 2);
    assert(strcmp(l1->last_key, "stale") == 0);
    assert(l1->last_reason == weaveffi_kv_EvictionReason_Expired);
    assert(l1->last_value_len == 2);

    // Replacing the listener frees the previous one; clearing frees the
    // current one.
    assert(g_listener_freed == 0);
    listener_ctx* l2 = new_listener(0, 0);
    weaveffi_kv_Store_set_eviction_listener(store, l2, &LISTENER_VTABLE, &err);
    assert(g_listener_freed == 1 && "replaced listener freed");
    weaveffi_kv_Store_clear_eviction_listener(store, &err);
    assert(err.code == 0);
    assert(g_listener_freed == 2 && "cleared listener freed");
    weaveffi_kv_Store_clear_eviction_listener(store, &err);
    assert(err.code == 0 && g_listener_freed == 2);

    // Nothing is attached now: a delete is not observed anywhere.
    put(store, "gamma", payload, 1);
    assert(weaveffi_kv_Store_delete(store, "gamma", &err));

    // A listener returning false detaches itself (and is freed).
    listener_ctx* l3 = new_listener(1, 0);
    weaveffi_kv_Store_set_eviction_listener(store, l3, &LISTENER_VTABLE, &err);
    put(store, "d1", payload, 1);
    put(store, "d2", payload, 1);
    assert(weaveffi_kv_Store_delete(store, "d1", &err));
    assert(g_listener_freed == 3 && "listener that answered false is freed");
    assert(weaveffi_kv_Store_delete(store, "d2", &err));
    assert(g_listener_freed == 3);

    // A listener that reports a foreign error aborts the delete with -4; the
    // store stays usable and still holds the listener until it is cleared.
    listener_ctx* l4 = new_listener(0, 1);
    weaveffi_kv_Store_set_eviction_listener(store, l4, &LISTENER_VTABLE, &err);
    put(store, "boom", payload, 1);
    weaveffi_kv_Store_delete(store, "boom", &err);
    assert(err.code == -4);
    assert(err.message != NULL && strstr(err.message, "listener exploded") != NULL);
    weaveffi_error_clear(&err);
    assert(count(store) == 1 && "the entry was removed before the listener ran");
    assert(g_listener_freed == 3);
    weaveffi_kv_Store_clear_eviction_listener(store, &err);
    assert(g_listener_freed == 4);

    // ── async compaction ─────────────────────────────────────────────────
    // An immediately-expired entry gives compact 3 bytes to reclaim. The raw
    // `_async` launcher returns immediately; completion arrives on the
    // producer's worker thread.
    put_ttl(store, "doomed", payload, sizeof payload, 0);
    weaveffi_kv_Store_compact_async(store, NULL, on_compact_done, NULL);
    wait_compact();
    assert(g_compact_err == 0);
    assert(g_compact_result == 3);
    assert(count(store) == 1);

    // A cancelled token makes compact fail with IoError through the
    // callback's heap-boxed error.
    weaveffi_cancel_token* token = weaveffi_cancel_token_create();
    assert(!weaveffi_cancel_token_is_cancelled(token));
    weaveffi_cancel_token_cancel(token);
    assert(weaveffi_cancel_token_is_cancelled(token));
    weaveffi_kv_Store_compact_async(store, token, on_compact_done, NULL);
    wait_compact();
    assert(g_compact_err == weaveffi_kv_KvError_IoError);
    weaveffi_cancel_token_destroy(token);

    // ── reference counting and the object graph ──────────────────────────
    // share() returns the very same object: identical pointer, and after the
    // original reference is released the shared one still sees the data.
    weaveffi_kv_Store* shared = weaveffi_kv_Store_share(store, &err);
    assert(err.code == 0);
    assert(shared == store && "share returns the same object");
    weaveffi_kv_Store_destroy(store);
    assert(count(shared) == 1 && "still alive through the shared reference");
    put(shared, "via-shared", payload, 1);
    assert(count(shared) == 2);
    store = shared;

    // clone: same pointer; destroy the original, the clone still works.
    weaveffi_kv_Store* cloned = weaveffi_kv_Store_clone(store);
    assert(cloned == store);
    weaveffi_kv_Store_destroy(store);
    assert(count(cloned) == 2 && "clone outlives the destroyed original");
    store = cloned;
    assert(weaveffi_kv_Store_clone(NULL) == NULL);
    weaveffi_kv_Store_destroy(NULL);

    // fork() is a distinct object with a copy of the entries.
    weaveffi_kv_Store* forked = weaveffi_kv_Store_fork(store, &err);
    assert(err.code == 0 && forked != NULL && forked != store);
    assert(count(forked) == 2);
    put(forked, "only-in-fork", payload, 1);
    assert(count(forked) == 3 && count(store) == 2);

    // larger(): `Store?` in and out. null means "none" both ways; a returned
    // object is an owned reference.
    weaveffi_kv_Store* empty = open_store("/tmp/conformance-kvstore-c-empty");
    assert(weaveffi_kv_Store_larger(empty, NULL, &err) == NULL);
    assert(err.code == 0);
    weaveffi_kv_Store* bigger = weaveffi_kv_Store_larger(empty, forked, &err);
    assert(bigger == forked);
    weaveffi_kv_Store_destroy(bigger);
    bigger = weaveffi_kv_Store_larger(forked, store, &err);
    assert(bigger == forked && "self wins when it holds more");
    weaveffi_kv_Store_destroy(bigger);
    bigger = weaveffi_kv_Store_larger(store, NULL, &err);
    assert(bigger == store && "a non-empty self is returned when other is absent");
    weaveffi_kv_Store_destroy(bigger);
    assert(count(forked) == 3 && "still alive after releasing the returned refs");

    // describe(): a record carrying the object itself (a token that is one
    // strong reference) and an optional object.
    const uint8_t* ibuf = weaveffi_kv_Store_describe(store, "primary", NULL, &out_len, &err);
    assert(err.code == 0 && ibuf != NULL);
    store_info_t info;
    wv_r_init(&r, ibuf, out_len);
    read_store_info(&r, &info);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ibuf, out_len);
    assert(strcmp(info.label, "primary") == 0);
    assert(info.store == store && "describe().store is the receiver");
    assert(info.mirror == NULL);
    assert(info.count == 2);
    free(info.label);
    weaveffi_kv_Store_destroy(info.store);  // release the adopted token

    ibuf = weaveffi_kv_Store_describe(forked, "with-mirror", store, &out_len, &err);
    assert(err.code == 0 && ibuf != NULL);
    wv_r_init(&r, ibuf, out_len);
    read_store_info(&r, &info);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ibuf, out_len);
    assert(strcmp(info.label, "with-mirror") == 0);
    assert(info.store == forked && info.mirror == store && info.count == 3);
    assert(count(info.mirror) == 2 && "the mirror token is a live reference");
    free(info.label);
    weaveffi_kv_Store_destroy(info.store);
    weaveffi_kv_Store_destroy(info.mirror);

    // open_many(): a list of objects as a return (u32 count + tokens).
    wv_writer paths;
    wv_w_init(&paths);
    wv_put_u32(&paths, 2);
    wv_put_str(&paths, "/a");
    wv_put_str(&paths, "/b");
    const uint8_t* mbuf = weaveffi_kv_Store_open_many(paths.buf, paths.len, &out_len, &err);
    assert(err.code == 0 && mbuf != NULL);
    wv_r_init(&r, mbuf, out_len);
    assert(wv_get_u32(&r) == 2);
    weaveffi_kv_Store* m0 = (weaveffi_kv_Store*)wv_get_obj(&r);
    weaveffi_kv_Store* m1 = (weaveffi_kv_Store*)wv_get_obj(&r);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)mbuf, out_len);
    wv_w_free(&paths);
    assert(m0 != NULL && m1 != NULL && m0 != m1);
    put(m0, "m", payload, 1);
    assert(count(m0) == 1 && count(m1) == 0);

    // A failing path fails the whole call with the typed code and no list.
    wv_w_init(&paths);
    wv_put_u32(&paths, 2);
    wv_put_str(&paths, "/ok");
    wv_put_str(&paths, "");
    assert(weaveffi_kv_Store_open_many(paths.buf, paths.len, &out_len, &err) == NULL);
    assert(err.code == weaveffi_kv_KvError_IoError);
    weaveffi_error_clear(&err);
    wv_w_free(&paths);

    // total_count(): objects inside a parameter list and inside an optional
    // record. Every token written is a fresh `_clone`, which the producer
    // adopts and drops; our own references stay valid afterward.
    wv_writer stores;
    wv_w_init(&stores);
    wv_put_u32(&stores, 3);
    wv_put_obj(&stores, weaveffi_kv_Store_clone(m0));
    wv_put_obj(&stores, weaveffi_kv_Store_clone(m1));
    wv_put_obj(&stores, weaveffi_kv_Store_clone(forked));
    wv_writer extra;
    wv_w_init(&extra);
    wv_put_bool(&extra, 1);
    write_store_info(&extra, "extra", store, forked, count(store));
    int64_t total = weaveffi_kv_Store_total_count(stores.buf, stores.len,
                                                  extra.buf, extra.len, &err);
    assert(err.code == 0);
    assert(total == 1 + 0 + 3 + 2);
    wv_w_free(&stores);
    wv_w_free(&extra);

    // ... and with the optional record absent, plus an empty list.
    wv_w_init(&stores);
    wv_put_u32(&stores, 1);
    wv_put_obj(&stores, weaveffi_kv_Store_clone(m0));
    const uint8_t none[1] = {0};
    assert(weaveffi_kv_Store_total_count(stores.buf, stores.len, none, 1, &err) == 1);
    wv_w_free(&stores);
    wv_w_init(&stores);
    wv_put_u32(&stores, 0);
    assert(weaveffi_kv_Store_total_count(stores.buf, stores.len, none, 1, &err) == 0);
    wv_w_free(&stores);

    // Everything we still hold is intact.
    assert(count(m0) == 1 && count(m1) == 0 && count(forked) == 3 && count(store) == 2);

    // clear() and release every reference exactly once.
    weaveffi_kv_Store_clear(forked, &err);
    assert(err.code == 0 && count(forked) == 0);
    weaveffi_kv_Store_destroy(m0);
    weaveffi_kv_Store_destroy(m1);
    weaveffi_kv_Store_destroy(empty);
    weaveffi_kv_Store_destroy(forked);
    weaveffi_kv_Store_destroy(store);

    // A store dropped with a listener attached frees the listener.
    weaveffi_kv_Store* with_listener = open_store("/tmp/conformance-kvstore-c-l");
    weaveffi_kv_Store_set_eviction_listener(with_listener, new_listener(0, 0),
                                            &LISTENER_VTABLE, &err);
    assert(g_listener_freed == 4);
    weaveffi_kv_Store_destroy(with_listener);
    assert(g_listener_freed == 5 && "dropping the store frees its listener");

    printf("c/kvstore: OK\n");
    return 0;
}
