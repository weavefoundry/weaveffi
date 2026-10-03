// Conformance consumer: kvstore sample, C target (ABI revision 3).
//
// Exercises the Store interface ABI end to end: the fallible constructor
// (`Store_open`), instance methods taking the receiver as the leading
// argument, the static method (`Store_default_capacity`), (ptr, len) string
// and bytes parameters, the typed error-domain codes surfaced through the
// error-out slot, the iterator's owned (ptr, len) items, the `Entry` record
// (list and map fields included) decoded through `kvstore_buffer.h`, the
// `kv.stats` submodule, the hand-written `EvictionListener` vtable (set,
// fire synchronously on delete and on expiry-on-read, replace, clear,
// detach, `free`), a consumer failure raised from the listener surfacing as
// code -4, the reference-counted object graph (`share`, `fork`,
// `clone`/`destroy`, `larger` with `Store?` both ways, `describe` returning
// a record that carries objects, `open_many` returning a list of objects,
// `total_count` taking objects inside a list and an optional record), and
// the cancellable async `compact`, whose cancelled call completes with -5.
// Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include <stdatomic.h>

#include "kvstore_buffer.h"

// ── eviction listener (consumer-implemented callback interface) ────────────

typedef struct {
    int evictions;
    char last_key[64];
    int32_t last_reason;
    size_t last_value_len;
    int detach_after;  // return false (detach) once this many evictions ran
    int fail;          // report a consumer failure instead of observing
} listener_ctx;

static int g_listener_freed = 0;

// The Entry buffer is borrowed for the duration of the dispatch.
static bool on_evict(void* ctx, const uint8_t* entry_ptr, size_t entry_len,
                     kvstore_kv_EvictionReason reason, kvstore_error* out_err) {
    listener_ctx* l = (listener_ctx*)ctx;
    if (l->fail) {
        kvstore_error_set(out_err, -4, "listener exploded");
        return true;
    }
    kvstore_kv_Entry e;
    assert(kvstore_kv_Entry_decode(entry_ptr, entry_len, &e));
    assert(e.id > 0 && e.created_at > 0);
    snprintf(l->last_key, sizeof l->last_key, "%s", e.key.ptr);
    l->last_value_len = e.value.len;
    kvstore_kv_Entry_free(&e);
    l->last_reason = (int32_t)reason;
    l->evictions++;
    return l->detach_after == 0 || l->evictions < l->detach_after;
}

static void listener_free(void* ctx) {
    free(ctx);
    g_listener_freed++;
}

static const kvstore_kv_EvictionListener_vtable LISTENER_VTABLE = {
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

static void on_compact_done(void* context, kvstore_error* err, int64_t result) {
    assert(context == (void*)0x5150);
    g_compact_err = err ? err->code : 0;
    kvstore_error_free(err);
    g_compact_result = result;
    atomic_store(&g_compact_done, 1);
}

static void wait_compact(void) {
    for (int i = 0; i < 5000 && !atomic_load(&g_compact_done); i++) sleep_ms(1);
    assert(atomic_load(&g_compact_done));
    atomic_store(&g_compact_done, 0);
}

// ── helpers ────────────────────────────────────────────────────────────────

// The `ttl_seconds: i64?` parameter, encoded present or absent.
typedef struct {
    kvstore_writer w;
} ttl_buf;

static ttl_buf ttl(const int64_t* seconds) {
    ttl_buf t;
    memset(&t, 0, sizeof t);
    kvstore_opt_i64_write(&t.w, seconds);
    return t;
}

static kvstore_kv_Store* open_store(const char* path) {
    kvstore_error err = {0};
    kvstore_kv_Store* s = kvstore_kv_Store_open(STR(path), &err);
    assert(err.code == 0 && s != NULL);
    return s;
}

static void put_ttl(kvstore_kv_Store* s, const char* key, const uint8_t* v, size_t n,
                    const int64_t* seconds) {
    kvstore_error err = {0};
    ttl_buf t = ttl(seconds);
    assert(kvstore_kv_Store_put(s, STR(key), v, n, kvstore_kv_EntryKind_Persistent, t.w.ptr,
                                t.w.len, &err));
    assert(err.code == 0);
    kvstore_writer_free(&t.w);
}

static void put(kvstore_kv_Store* s, const char* key, const uint8_t* v, size_t n) {
    put_ttl(s, key, v, n, NULL);
}

static int64_t count(const kvstore_kv_Store* s) {
    kvstore_error err = {0};
    int64_t n = kvstore_kv_Store_count(s, &err);
    assert(err.code == 0);
    return n;
}

// Collect list_keys(prefix?) into `out`; each key is an owned string.
static int list_keys(kvstore_kv_Store* s, const char* prefix, char** out, int cap) {
    kvstore_error err = {0};
    kvstore_str view = kvstore_str_of(prefix);
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_opt_string_write(&w, prefix ? &view : NULL);
    kvstore_kv_Store_ListKeysIterator* it = kvstore_kv_Store_list_keys(s, w.ptr, w.len, &err);
    kvstore_writer_free(&w);
    assert(err.code == 0 && it != NULL);
    int n = 0;
    const uint8_t* item = NULL;
    size_t len = 0;
    while (kvstore_kv_Store_ListKeysIterator_next(it, &item, &len, &err) == 1) {
        assert(n < cap);
        out[n] = (char*)calloc(len + 1, 1);
        memcpy(out[n++], item, len);
        kvstore_free_bytes((uint8_t*)item, len);
    }
    assert(err.code == 0);
    kvstore_kv_Store_ListKeysIterator_destroy(it);
    return n;
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

static void basics(kvstore_kv_Store* store) {
    kvstore_error err = {0};
    const uint8_t payload[3] = {1, 2, 3};

    // Populate two keys so count/iterator/stats have something to report.
    put(store, "alpha", BYTES(payload));
    ttl_buf none = ttl(NULL);
    assert(kvstore_kv_Store_put(store, STR("beta"), BYTES(payload),
                                kvstore_kv_EntryKind_Volatile, none.w.ptr, none.w.len, &err));
    assert(err.code == 0);
    assert(count(store) == 2);

    // An out-of-range enum discriminant is a marshalling failure (-3).
    assert(!kvstore_kv_Store_put(store, STR("bad"), BYTES(payload), (kvstore_kv_EntryKind)999,
                                 none.w.ptr, none.w.len, &err));
    assert(err.code == -3);
    kvstore_error_clear(&err);
    // So is invalid UTF-8 in a string parameter.
    const uint8_t bad_utf8[2] = {0xC3, 0x28};
    assert(!kvstore_kv_Store_put(store, BYTES(bad_utf8), BYTES(payload),
                                 kvstore_kv_EntryKind_Volatile, none.w.ptr, none.w.len, &err));
    assert(err.code == -3);
    kvstore_error_clear(&err);
    kvstore_writer_free(&none.w);
    assert(count(store) == 2);

    // Typed error path on a method: a missing key reports KeyNotFound.
    size_t len = 0;
    assert(kvstore_kv_Store_get(store, STR("missing"), &len, &err) == NULL);
    assert(err.code == kvstore_kv_KvError_KeyNotFound);
    assert(strcmp(err.message, "key not found") == 0);
    kvstore_error_clear(&err);

    // The deprecated method still works; the header marks it deprecated, so
    // silence the warning for this one deliberate call.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    assert(kvstore_kv_Store_legacy_put(store, STR("old"), BYTES(payload), &err));
#pragma clang diagnostic pop
    assert(err.code == 0);
    assert(kvstore_kv_Store_delete(store, STR("old"), &err) && err.code == 0);
    assert(!kvstore_kv_Store_delete(store, STR("old"), &err) && err.code == 0);

    // Iterator: keys come back sorted, filtered by the optional prefix.
    char* keys[4];
    int n = list_keys(store, NULL, keys, 4);
    assert(n == 2 && strcmp(keys[0], "alpha") == 0 && strcmp(keys[1], "beta") == 0);
    for (int i = 0; i < n; i++) free(keys[i]);
    n = list_keys(store, "be", keys, 4);
    assert(n == 1 && strcmp(keys[0], "beta") == 0);
    free(keys[0]);
    assert(list_keys(store, "zzz", keys, 4) == 0);

    // get -> buffered `Entry?` with nested list and map fields.
    const uint8_t* buf = kvstore_kv_Store_get(store, STR("alpha"), &len, &err);
    assert(err.code == 0 && buf != NULL);
    kvstore_kv_Entry* e = NULL;
    assert(kvstore_opt_kv_Entry_decode(buf, len, &e));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(e != NULL);
    assert(e->id == 1 && "first id handed out by this store");
    assert(strcmp(e->key.ptr, "alpha") == 0);
    assert(e->value.len == 3 && memcmp(e->value.ptr, payload, 3) == 0);
    assert(e->created_at > 0);
    assert(e->expires_at == NULL && "no TTL");
    assert(e->tags.len == 0 && e->metadata.len == 0);
    kvstore_opt_kv_Entry_free(&e);

    // kv.stats submodule: takes the parent module's Store.
    buf = kvstore_kv_stats_get_stats(store, &len, &err);
    assert(err.code == 0 && buf != NULL);
    kvstore_kv_stats_Stats stats;
    assert(kvstore_kv_stats_Stats_decode(buf, len, &stats));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(stats.total_entries == 2);
    assert(stats.total_bytes == 6 && "two 3-byte values");
    assert(stats.expired_entries == 0);
}

static void listeners(kvstore_kv_Store* store) {
    kvstore_error err = {0};
    const uint8_t payload[3] = {1, 2, 3};
    size_t len = 0;

    // delete fires on_evict synchronously with the Entry and reason Deleted.
    listener_ctx* l1 = new_listener(0, 0);
    kvstore_kv_Store_set_eviction_listener(store, l1, &LISTENER_VTABLE, &err);
    assert(err.code == 0);
    assert(kvstore_kv_Store_delete(store, STR("beta"), &err) && err.code == 0);
    assert(l1->evictions == 1);
    assert(strcmp(l1->last_key, "beta") == 0);
    assert(l1->last_reason == kvstore_kv_EvictionReason_Deleted);
    assert(l1->last_value_len == 3);

    // An expired entry is evicted on read: get reports Expired and the
    // listener sees reason Expired.
    const int64_t past = -1;
    put_ttl(store, "stale", payload, 2, &past);
    assert(kvstore_kv_Store_get(store, STR("stale"), &len, &err) == NULL);
    assert(err.code == kvstore_kv_KvError_Expired);
    kvstore_error_clear(&err);
    assert(l1->evictions == 2);
    assert(strcmp(l1->last_key, "stale") == 0);
    assert(l1->last_reason == kvstore_kv_EvictionReason_Expired);
    assert(l1->last_value_len == 2);

    // Replacing the listener frees the previous one; clearing frees the
    // current one.
    assert(g_listener_freed == 0);
    kvstore_kv_Store_set_eviction_listener(store, new_listener(0, 0), &LISTENER_VTABLE, &err);
    assert(g_listener_freed == 1 && "replaced listener freed");
    kvstore_kv_Store_clear_eviction_listener(store, &err);
    assert(err.code == 0 && g_listener_freed == 2);
    kvstore_kv_Store_clear_eviction_listener(store, &err);
    assert(err.code == 0 && g_listener_freed == 2);

    // Nothing is attached now: a delete is not observed anywhere.
    put(store, "gamma", payload, 1);
    assert(kvstore_kv_Store_delete(store, STR("gamma"), &err));

    // A listener returning false detaches itself (and is freed).
    kvstore_kv_Store_set_eviction_listener(store, new_listener(1, 0), &LISTENER_VTABLE, &err);
    put(store, "d1", payload, 1);
    put(store, "d2", payload, 1);
    assert(kvstore_kv_Store_delete(store, STR("d1"), &err));
    assert(g_listener_freed == 3 && "listener that answered false is freed");
    assert(kvstore_kv_Store_delete(store, STR("d2"), &err));
    assert(g_listener_freed == 3);

    // A listener that reports a failure aborts the delete with -4; the store
    // stays usable and still holds the listener until it is cleared.
    kvstore_kv_Store_set_eviction_listener(store, new_listener(0, 1), &LISTENER_VTABLE, &err);
    put(store, "boom", payload, 1);
    kvstore_kv_Store_delete(store, STR("boom"), &err);
    assert(err.code == -4);
    assert(err.message != NULL && strstr(err.message, "listener exploded") != NULL);
    kvstore_error_clear(&err);
    assert(count(store) == 1 && "the entry was removed before the listener ran");
    assert(g_listener_freed == 3);
    kvstore_kv_Store_clear_eviction_listener(store, &err);
    assert(g_listener_freed == 4);
}

static void compaction(kvstore_kv_Store* store) {
    const uint8_t payload[3] = {1, 2, 3};

    // An immediately-expired entry gives compact 3 bytes to reclaim. The
    // launcher returns at once; completion arrives on a producer thread.
    const int64_t now = 0;
    put_ttl(store, "doomed", BYTES(payload), &now);
    kvstore_kv_Store_compact(store, NULL, on_compact_done, (void*)0x5150);
    wait_compact();
    assert(g_compact_err == 0);
    assert(g_compact_result == 3);
    assert(count(store) == 1);

    // With a live token, compaction runs normally.
    kvstore_cancel_token* token = kvstore_cancel_token_create();
    kvstore_kv_Store_compact(store, token, on_compact_done, (void*)0x5150);
    wait_compact();
    assert(g_compact_err == 0 && g_compact_result == 0);
    kvstore_cancel_token_destroy(token);

    // A cancelled token completes the call with the cancelled code (-5)
    // through the heap-boxed error.
    token = kvstore_cancel_token_create();
    assert(!kvstore_cancel_token_is_cancelled(token));
    kvstore_cancel_token_cancel(token);
    assert(kvstore_cancel_token_is_cancelled(token));
    kvstore_kv_Store_compact(store, token, on_compact_done, (void*)0x5150);
    kvstore_cancel_token_destroy(token);
    wait_compact();
    assert(g_compact_err == -5);
    assert(count(store) == 1);
}

static void object_graph(kvstore_kv_Store* store) {
    kvstore_error err = {0};
    const uint8_t payload[3] = {1, 2, 3};
    size_t len = 0;

    // share() returns the very same object; after the original reference is
    // released the shared one still sees the data.
    kvstore_kv_Store* shared = kvstore_kv_Store_share(store, &err);
    assert(err.code == 0 && shared == store);
    kvstore_kv_Store_destroy(store);
    assert(count(shared) == 1 && "still alive through the shared reference");
    put(shared, "via-shared", payload, 1);
    assert(count(shared) == 2);
    store = shared;

    // clone: same pointer; destroy the original, the clone still works.
    kvstore_kv_Store* cloned = kvstore_kv_Store_clone(store);
    assert(cloned == store);
    kvstore_kv_Store_destroy(store);
    assert(count(cloned) == 2 && "clone outlives the destroyed original");
    store = cloned;
    assert(kvstore_kv_Store_clone(NULL) == NULL);
    kvstore_kv_Store_destroy(NULL);

    // fork() is a distinct object with a copy of the entries.
    kvstore_kv_Store* forked = kvstore_kv_Store_fork(store, &err);
    assert(err.code == 0 && forked != NULL && forked != store);
    assert(count(forked) == 2);
    put(forked, "only-in-fork", payload, 1);
    assert(count(forked) == 3 && count(store) == 2);

    // larger(): `Store?` in and out; NULL is "none" both ways, and a returned
    // object is an owned reference.
    kvstore_kv_Store* empty = open_store("/tmp/conformance-kvstore-c-empty");
    assert(kvstore_kv_Store_larger(empty, NULL, &err) == NULL);
    assert(err.code == 0);
    kvstore_kv_Store* bigger = kvstore_kv_Store_larger(empty, forked, &err);
    assert(bigger == forked);
    kvstore_kv_Store_destroy(bigger);
    bigger = kvstore_kv_Store_larger(forked, store, &err);
    assert(bigger == forked && "self wins when it holds more");
    kvstore_kv_Store_destroy(bigger);
    bigger = kvstore_kv_Store_larger(store, NULL, &err);
    assert(bigger == store && "a non-empty self is returned when other is absent");
    kvstore_kv_Store_destroy(bigger);

    // describe(): a record carrying the object itself and an optional object.
    kvstore_kv_StoreInfo info = describe(store, "primary", NULL);
    assert(strcmp(info.label.ptr, "primary") == 0);
    assert(info.store == store && "describe().store is the receiver");
    assert(info.mirror == NULL && info.count == 2);
    kvstore_kv_StoreInfo_free(&info);  // releases the adopted reference

    info = describe(forked, "with-mirror", store);
    assert(info.store == forked && info.mirror == store && info.count == 3);
    assert(count(info.mirror) == 2 && "the mirror token is a live reference");
    kvstore_kv_StoreInfo_free(&info);

    // open_many(): a list of objects as a return.
    kvstore_str paths[2] = {kvstore_str_of("/a"), kvstore_str_of("/b")};
    kvstore_list_string path_list;
    path_list.items = paths;
    path_list.len = 2;
    kvstore_writer w;
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &path_list);
    const uint8_t* buf = kvstore_kv_Store_open_many(w.ptr, w.len, &len, &err);
    kvstore_writer_free(&w);
    assert(err.code == 0 && buf != NULL);
    kvstore_list_kv_Store many;
    assert(kvstore_list_kv_Store_decode(buf, len, &many));
    kvstore_free_bytes((uint8_t*)buf, len);
    assert(many.len == 2 && many.items[0] != many.items[1]);
    put(many.items[0], "m", payload, 1);
    assert(count(many.items[0]) == 1 && count(many.items[1]) == 0);

    // A failing path fails the whole call with the typed code and no list.
    paths[1] = kvstore_str_of("");
    memset(&w, 0, sizeof w);
    kvstore_list_string_write(&w, &path_list);
    assert(kvstore_kv_Store_open_many(w.ptr, w.len, &len, &err) == NULL);
    kvstore_writer_free(&w);
    assert(err.code == kvstore_kv_KvError_IoError);
    assert(strcmp(err.message, "I/O failure") == 0);
    kvstore_error_clear(&err);

    // total_count(): objects inside a list and inside an optional record.
    // The writer stores a fresh `_clone` per object, which the producer
    // adopts and drops; our own references stay valid.
    kvstore_kv_Store* three[3] = {many.items[0], many.items[1], forked};
    kvstore_list_kv_Store stores;
    stores.items = three;
    stores.len = 3;
    kvstore_kv_StoreInfo extra;
    extra.label = kvstore_str_of("extra");
    extra.store = store;
    extra.mirror = forked;
    extra.count = count(store);
    kvstore_writer ws, we;
    memset(&ws, 0, sizeof ws);
    memset(&we, 0, sizeof we);
    kvstore_list_kv_Store_write(&ws, &stores);
    kvstore_opt_kv_StoreInfo_write(&we, &extra);
    assert(kvstore_kv_Store_total_count(ws.ptr, ws.len, we.ptr, we.len, &err) ==
           1 + 0 + 3 + 2);
    assert(err.code == 0);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&we);

    // ... and with the optional record absent, plus an empty list.
    stores.len = 1;
    memset(&ws, 0, sizeof ws);
    memset(&we, 0, sizeof we);
    kvstore_list_kv_Store_write(&ws, &stores);
    kvstore_opt_kv_StoreInfo_write(&we, NULL);
    assert(kvstore_kv_Store_total_count(ws.ptr, ws.len, we.ptr, we.len, &err) == 1);
    kvstore_writer_free(&ws);
    stores.len = 0;
    memset(&ws, 0, sizeof ws);
    kvstore_list_kv_Store_write(&ws, &stores);
    assert(kvstore_kv_Store_total_count(ws.ptr, ws.len, we.ptr, we.len, &err) == 0);
    kvstore_writer_free(&ws);
    kvstore_writer_free(&we);

    // Everything we still hold is intact.
    assert(count(many.items[0]) == 1 && count(forked) == 3 && count(store) == 2);

    // clear() and release every reference exactly once.
    kvstore_kv_Store_clear(forked, &err);
    assert(err.code == 0 && count(forked) == 0);
    kvstore_list_kv_Store_free(&many);  // destroys both opened stores
    kvstore_kv_Store_destroy(empty);
    kvstore_kv_Store_destroy(forked);
    kvstore_kv_Store_destroy(store);
}

int main(void) {
    kvstore_error err = {0};

    assert(KVSTORE_ABI_VERSION == 3u && kvstore_abi_version() == KVSTORE_ABI_VERSION);
    assert(kvstore_kv_checksum() == KVSTORE_KV_CHECKSUM);

    // Static method: no receiver, plain error-out slot.
    assert(kvstore_kv_Store_default_capacity(&err) == 1000000);
    assert(err.code == 0);

    // Fallible constructor, typed error path: an empty path reports IoError
    // with the error's Display message.
    assert(kvstore_kv_Store_open(STR(""), &err) == NULL);
    assert(err.code == kvstore_kv_KvError_IoError);
    assert(err.message != NULL && strcmp(err.message, "I/O failure") == 0);
    kvstore_error_clear(&err);

    // A NULL string pointer with a nonzero length is a marshalling failure.
    assert(kvstore_kv_Store_open(NULL, 4, &err) == NULL);
    assert(err.code == -3);
    kvstore_error_clear(&err);

    kvstore_kv_Store* store = open_store("/tmp/conformance-kvstore-c");
    basics(store);
    listeners(store);
    compaction(store);
    object_graph(store);  // consumes `store`

    // A store dropped with a listener attached frees the listener.
    kvstore_kv_Store* with_listener = open_store("/tmp/conformance-kvstore-c-l");
    kvstore_kv_Store_set_eviction_listener(with_listener, new_listener(0, 0), &LISTENER_VTABLE,
                                           &err);
    assert(g_listener_freed == 4);
    kvstore_kv_Store_destroy(with_listener);
    assert(g_listener_freed == 5 && "dropping the store frees its listener");

    ASSERT_NO_LEAKS(kvstore_debug_live);
    printf("c/kvstore: OK\n");
    return 0;
}
