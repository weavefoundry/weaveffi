// Conformance consumer: kvstore sample, C target.
//
// Exercises the Store interface ABI end to end: the fallible constructor
// (`Store_open`), instance methods taking the receiver as the leading
// argument, the static method (`Store_default_capacity`), the typed
// error-domain codes surfaced through the error-out slot, the iterator
// out-param `next` convention, the `Entry` record's list/map getters, the
// `kv.stats` submodule, the raw listener registration ABI (register -> fire
// synchronously on delete -> unregister), and the raw `_async` launcher whose
// completion callback arrives on the producer's worker thread (synchronized
// here with C11 atomics). Exits 0 on success; aborts otherwise.

#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "weaveffi.h"

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

int main(void) {
    weaveffi_error err = {0, NULL};

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

    // Populate two keys so count/iterator/stats have something to report.
    const uint8_t payload[3] = {1, 2, 3};
    assert(weaveffi_kv_Store_put(store, "alpha", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Persistent, NULL, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_Store_put(store, "beta", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Volatile, NULL, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_Store_count(store, &err) == 2);

    // Typed error path on a method: a missing key reports KeyNotFound (1001).
    weaveffi_kv_Entry* nope = weaveffi_kv_Store_get(store, "missing", &err);
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

    // Iterator: `next` writes one element per call and returns 1/0.
    weaveffi_kv_Store_ListKeysIterator* it =
        weaveffi_kv_Store_list_keys(store, NULL, &err);
    assert(err.code == 0 && it != NULL);
    int seen_alpha = 0, seen_beta = 0, n = 0;
    const char* item = NULL;
    weaveffi_error iter_err = {0, NULL};
    while (weaveffi_kv_Store_ListKeysIterator_next(it, &item, &iter_err) != 0) {
        if (strcmp(item, "alpha") == 0) seen_alpha = 1;
        if (strcmp(item, "beta") == 0) seen_beta = 1;
        weaveffi_free_string(item);
        n++;
    }
    assert(iter_err.code == 0);
    weaveffi_kv_Store_ListKeysIterator_destroy(it);
    assert(n == 2 && seen_alpha && seen_beta);

    // Build an entry carrying a non-empty list and map so the getters return
    // producer-allocated arrays.
    const char* tags[2] = {"hot", "fast"};
    const char* mkeys[2] = {"source", "env"};
    const char* mvals[2] = {"test", "prod"};
    weaveffi_kv_Entry* e = weaveffi_kv_Entry_create(
        7, "alpha", payload, sizeof payload, 1000, NULL,
        tags, 2, mkeys, mvals, 2, &err);
    assert(err.code == 0 && e != NULL);
    assert(weaveffi_kv_Entry_get_id(e) == 7);

    // List getter: [string] -> const char** + out_len.
    size_t tlen = 0;
    const char** got_tags = weaveffi_kv_Entry_get_tags(e, &tlen);
    assert(tlen == 2 && got_tags != NULL);
    int seen_hot = 0, seen_fast = 0;
    for (size_t i = 0; i < tlen; i++) {
        if (strcmp(got_tags[i], "hot") == 0) seen_hot = 1;
        if (strcmp(got_tags[i], "fast") == 0) seen_fast = 1;
        weaveffi_free_string(got_tags[i]);
    }
    assert(seen_hot && seen_fast);

    // Map getter: the triple-pointer out-params. `&keys` is `const char***`.
    const char** keys = NULL;
    const char** vals = NULL;
    size_t mlen = 0;
    weaveffi_kv_Entry_get_metadata(e, &keys, &vals, &mlen);
    assert(mlen == 2 && keys != NULL && vals != NULL);
    int ok_source = 0, ok_env = 0;
    for (size_t i = 0; i < mlen; i++) {
        if (strcmp(keys[i], "source") == 0 && strcmp(vals[i], "test") == 0) ok_source = 1;
        if (strcmp(keys[i], "env") == 0 && strcmp(vals[i], "prod") == 0) ok_env = 1;
        weaveffi_free_string(keys[i]);
        weaveffi_free_string(vals[i]);
    }
    assert(ok_source && ok_env);
    weaveffi_kv_Entry_destroy(e);

    // Empty map round-trips as len 0 (the null-array branch).
    weaveffi_kv_Entry* empty = weaveffi_kv_Entry_create(
        8, "k", payload, sizeof payload, 1000, NULL, NULL, 0, NULL, NULL, 0, &err);
    assert(err.code == 0 && empty != NULL);
    const char** ek = NULL;
    const char** ev = NULL;
    size_t elen = 99;
    weaveffi_kv_Entry_get_metadata(empty, &ek, &ev, &elen);
    assert(elen == 0);
    weaveffi_kv_Entry_destroy(empty);

    // kv.stats submodule: snapshot stats for the interface-typed param.
    weaveffi_kv_stats_Stats* st = weaveffi_kv_stats_get_stats(store, &err);
    assert(err.code == 0 && st != NULL);
    assert(weaveffi_kv_stats_Stats_get_total_entries(st) == 2);
    weaveffi_kv_stats_Stats_destroy(st);

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
    int64_t zero_ttl = 0;
    assert(weaveffi_kv_Store_put(store, "doomed", payload, sizeof payload,
                                 weaveffi_kv_EntryKind_Volatile, &zero_ttl, &err));
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
