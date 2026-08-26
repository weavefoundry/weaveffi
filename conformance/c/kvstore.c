// Conformance consumer: kvstore sample, C target.
//
// Exercises the Store interface ABI end to end: the fallible constructor
// (`Store_open`), instance methods taking the receiver as the leading
// argument, the static method (`Store_default_capacity`), the typed
// error-domain codes surfaced through the error-out slot, the iterator
// out-param `next` convention, the `Entry` record decoded from a value
// buffer (list and map fields included), the `kv.stats` submodule, the raw
// listener registration ABI (register -> fire synchronously on delete ->
// unregister), and the raw `_async` launcher whose completion callback
// arrives on the producer's worker thread (synchronized here with C11
// atomics). Exits 0 on success; aborts otherwise.

#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "weaveffi.h"
#include "wvbuf.h"

// ── eviction listener state ────────────────────────────────────────────────
static int g_evictions = 0;
static char g_last_evicted[64];

static void on_evict(const char* key, void* context) {
    (void)context;
    g_evictions++;
    snprintf(g_last_evicted, sizeof g_last_evicted, "%s", key);
}

// ── async completion state ─────────────────────────────────────────────────
static atomic_int g_compact_done = 0;
static int64_t g_compact_result = -1;
static int32_t g_compact_err = -1;

static void on_compact_done(void* context, weaveffi_error* err, int64_t result) {
    (void)context;
    g_compact_err = err ? err->code : 0;
    g_compact_result = result;
    atomic_store(&g_compact_done, 1);
}

// Optional-i64 parameter encodings for `ttl_seconds`.
static const uint8_t TTL_NONE[1] = {0};

int main(void) {
    weaveffi_error err = {0};

    // Static method: no receiver, plain error-out slot.
    assert(weaveffi_kv_Store_default_capacity(&err) == 1000000);
    assert(err.code == 0);

    // Fallible constructor, typed error path: an empty path reports the
    // KvError.IoError domain code (1004).
    weaveffi_kv_Store* bad = weaveffi_kv_Store_open("", &err);
    assert(bad == NULL);
    assert(err.code == weaveffi_kv_KvError_IoError);
    weaveffi_error_clear(&err);

    weaveffi_kv_Store* store =
        weaveffi_kv_Store_open("/tmp/conformance-kvstore-c", &err);
    assert(err.code == 0);
    assert(store != NULL);

    // Populate two keys so count/iterator/stats have something to report. The
    // optional TTL parameter is a buffered `i64?` (flag byte + payload).
    const uint8_t payload[3] = {1, 2, 3};
    assert(weaveffi_kv_Store_put(store, "alpha", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Persistent,
                                 TTL_NONE, sizeof TTL_NONE, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_Store_put(store, "beta", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Volatile,
                                 TTL_NONE, sizeof TTL_NONE, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_Store_count(store, &err) == 2);

    // Typed error path on a method: a missing key reports KeyNotFound (1001).
    size_t out_len = 0;
    const uint8_t* nope = weaveffi_kv_Store_get(store, "missing", &out_len, &err);
    assert(nope == NULL);
    assert(err.code == weaveffi_kv_KvError_KeyNotFound);
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

    // Iterator: `next` writes one element per call and returns 1/0. The
    // optional prefix parameter is a buffered `string?`; absent here.
    weaveffi_kv_Store_ListKeysIterator* it =
        weaveffi_kv_Store_list_keys(store, TTL_NONE, sizeof TTL_NONE, &err);
    assert(err.code == 0 && it != NULL);
    int seen_alpha = 0, seen_beta = 0, n = 0;
    const char* item = NULL;
    weaveffi_error iter_err = {0};
    while (weaveffi_kv_Store_ListKeysIterator_next(it, &item, &iter_err) != 0) {
        if (strcmp(item, "alpha") == 0) seen_alpha = 1;
        if (strcmp(item, "beta") == 0) seen_beta = 1;
        weaveffi_free_string(item);
        n++;
    }
    assert(iter_err.code == 0);
    weaveffi_kv_Store_ListKeysIterator_destroy(it);
    assert(n == 2 && seen_alpha && seen_beta);

    // get -> buffered `Entry?`: flag byte, then the record's fields in
    // declaration order (id, key, value, created_at, expires_at?, tags,
    // metadata). The list and map fields nest inside the same buffer.
    const uint8_t* ebuf = weaveffi_kv_Store_get(store, "alpha", &out_len, &err);
    assert(err.code == 0 && ebuf != NULL);
    wv_reader r;
    wv_r_init(&r, ebuf, out_len);
    assert(wv_get_bool(&r) == 1 && "entry present");
    int64_t id = wv_get_i64(&r);
    assert(id > 0);
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
    uint32_t tag_count = wv_get_u32(&r);
    for (uint32_t i = 0; i < tag_count; i++) free(wv_get_str(&r));
    uint32_t meta_count = wv_get_u32(&r);
    for (uint32_t i = 0; i < meta_count; i++) {
        free(wv_get_str(&r));
        free(wv_get_str(&r));
    }
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ebuf, out_len);

    // kv.stats submodule: the Stats record is three i64 fields in one buffer.
    const uint8_t* sbuf = weaveffi_kv_stats_get_stats(store, &out_len, &err);
    assert(err.code == 0 && sbuf != NULL);
    wv_r_init(&r, sbuf, out_len);
    assert(wv_get_i64(&r) == 2 && "total_entries");
    assert(wv_get_i64(&r) == 6 && "total_bytes: two 3-byte values");
    wv_get_i64(&r);  // expired_entries: timing-dependent, just consume it.
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)sbuf, out_len);

    // Eviction listener: delete fires the raw callback synchronously on the
    // calling thread.
    uint64_t sub = weaveffi_kv_register_eviction_listener(on_evict, NULL);
    assert(sub > 0);
    assert(weaveffi_kv_Store_delete(store, "beta", &err) && err.code == 0);
    assert(g_evictions == 1 && strcmp(g_last_evicted, "beta") == 0);

    // Unregister stops delivery.
    weaveffi_kv_unregister_eviction_listener(sub);
    assert(weaveffi_kv_Store_delete(store, "alpha", &err) && err.code == 0);
    assert(g_evictions == 1);

    // Async method: an immediately-expired entry gives compact 3 bytes to
    // reclaim. The raw `_async` launcher returns immediately; completion
    // arrives on the producer's worker thread, so poll the atomic flag.
    uint8_t ttl_zero[9];
    ttl_zero[0] = 1;
    memset(ttl_zero + 1, 0, 8);  // Some(0): flag byte + little-endian i64 0.
    assert(weaveffi_kv_Store_put(store, "doomed", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Volatile,
                                 ttl_zero, sizeof ttl_zero, &err));
    assert(err.code == 0);
    weaveffi_kv_Store_compact_async(store, NULL, on_compact_done, NULL);
    for (int i = 0; i < 5000 && !atomic_load(&g_compact_done); i++) usleep(1000);
    assert(atomic_load(&g_compact_done));
    assert(g_compact_err == 0);
    assert(g_compact_result == 3);
    assert(weaveffi_kv_Store_count(store, &err) == 0 && err.code == 0);

    weaveffi_kv_Store_destroy(store);

    printf("c/kvstore: OK\n");
    return 0;
}
