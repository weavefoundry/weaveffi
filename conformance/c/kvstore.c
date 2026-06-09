// Conformance consumer: kvstore sample, C target.
//
// Exercises the map return-by-value ABI redesign end to end: a returned
// `{string:string}` lowers to `const char*** out_keys, const char*** out_values,
// size_t* out_len`, and the caller passes the address of a `const char**` it owns.
// Also covers the `[string]` list getter, the iterator out-param `next`
// convention, and the `kv.stats` submodule. Exits 0 on success; aborts otherwise.

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"

int main(void) {
    weaveffi_error err = {0, NULL};

    weaveffi_kv_Store* store = weaveffi_kv_open_store("/tmp/conformance-kvstore-c", &err);
    assert(err.code == 0);
    assert(store != NULL);

    // Populate two keys so count/iterator/stats have something to report.
    const uint8_t payload[3] = {1, 2, 3};
    assert(weaveffi_kv_put(store, "alpha", payload, sizeof payload,
                           weaveffi_kv_EntryKind_Persistent, NULL, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_put(store, "beta", payload, sizeof payload,
                           weaveffi_kv_EntryKind_Volatile, NULL, &err));
    assert(err.code == 0);
    assert(weaveffi_kv_count(store, &err) == 2);

    // Iterator: `next` writes one element per call and returns 1/0.
    weaveffi_kv_ListKeysIterator* it = weaveffi_kv_list_keys(store, NULL, &err);
    assert(err.code == 0 && it != NULL);
    int seen_alpha = 0, seen_beta = 0, n = 0;
    const char* item = NULL;
    weaveffi_error iter_err = {0, NULL};
    while (weaveffi_kv_ListKeysIterator_next(it, &item, &iter_err) != 0) {
        if (strcmp(item, "alpha") == 0) seen_alpha = 1;
        if (strcmp(item, "beta") == 0) seen_beta = 1;
        weaveffi_free_string(item);
        n++;
    }
    assert(iter_err.code == 0);
    weaveffi_kv_ListKeysIterator_destroy(it);
    assert(n == 2 && seen_alpha && seen_beta);

    // Build an entry carrying a non-empty list and map so the getters return
    // producer-allocated arrays (the case the ABI redesign fixes).
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

    // kv.stats submodule: snapshot stats.
    weaveffi_kv_stats_Stats* st = weaveffi_kv_stats_get_stats(store, &err);
    assert(err.code == 0 && st != NULL);
    assert(weaveffi_kv_stats_Stats_get_total_entries(st) == 2);
    weaveffi_kv_stats_Stats_destroy(st);

    weaveffi_kv_close_store(store, &err);
    assert(err.code == 0);

    printf("c/kvstore: OK\n");
    return 0;
}
