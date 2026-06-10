// Conformance consumer: kvstore sample, C++ target.
//
// Drives the generated header-only wrappers, focusing on the map return-by-value
// redesign: `Entry::metadata()` reads the triple-pointer out-params into an
// `unordered_map`. Also covers the `[string]` list getter, the builder's
// list/map *input* marshaling, the iterator-backed `kv_list_keys`, the
// `kv.stats` submodule wrapper, the std::function eviction listener (register
// -> fire synchronously on delete -> unregister), and the std::future-backed
// `kv_compact_async` settled from the producer's worker thread. Aborts
// (non-zero) on any failed assertion.

#include <cassert>
#include <cstdio>
#include <future>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "weaveffi.hpp"

using namespace kvstore;

int main() {
    Store store = kv_open_store("/tmp/conformance-kvstore-cpp");

    const std::vector<uint8_t> payload{1, 2, 3};
    assert(kv_put(store, "alpha", payload, EntryKind::Persistent, std::nullopt));
    assert(kv_put(store, "beta", payload, EntryKind::Volatile, std::nullopt));
    assert(kv_count(store) == 2);

    // Iterator-backed listing materialized into a vector.
    std::vector<std::string> keys = kv_list_keys(store, std::nullopt);
    assert(keys.size() == 2);
    bool saw_alpha = false, saw_beta = false;
    for (const auto& k : keys) {
        if (k == "alpha") saw_alpha = true;
        if (k == "beta") saw_beta = true;
    }
    assert(saw_alpha && saw_beta);

    // Builder marshals a [string] list and a {string:string} map *in*; the
    // getters then read them back *out* (the case the ABI redesign fixes).
    Entry entry = EntryBuilder()
                      .withId(7)
                      .withKey("alpha")
                      .withValue(payload)
                      .withCreatedAt(1000)
                      .withExpiresAt(std::nullopt)
                      .withTags({"hot", "fast"})
                      .withMetadata({{"source", "test"}, {"env", "prod"}})
                      .build();
    assert(entry.id() == 7);

    std::vector<std::string> tags = entry.tags();
    assert(tags.size() == 2);
    bool saw_hot = false, saw_fast = false;
    for (const auto& t : tags) {
        if (t == "hot") saw_hot = true;
        if (t == "fast") saw_fast = true;
    }
    assert(saw_hot && saw_fast);

    std::unordered_map<std::string, std::string> md = entry.metadata();
    assert(md.size() == 2);
    assert(md.at("source") == "test");
    assert(md.at("env") == "prod");

    // Empty map round-trips as an empty map (the null-array branch).
    Entry empty = EntryBuilder()
                      .withId(8)
                      .withKey("k")
                      .withValue(payload)
                      .withCreatedAt(1)
                      .withExpiresAt(std::nullopt)
                      .withTags({})
                      .withMetadata({})
                      .build();
    assert(empty.metadata().empty());

    // kv.stats submodule wrapper.
    Stats st = kv_stats_get_stats(store);
    assert(st.total_entries() == 2);

    // Eviction listener: delete fires the std::function trampoline
    // synchronously on the calling thread.
    std::vector<std::string> evicted;
    uint64_t sub = kv_register_eviction_listener(
        [&evicted](std::string key) { evicted.push_back(std::move(key)); });
    assert(sub > 0);
    assert(kv_delete(store, "beta"));
    assert(evicted.size() == 1 && evicted[0] == "beta");

    // Unregister stops delivery.
    kv_unregister_eviction_listener(sub);
    assert(kv_delete(store, "alpha"));
    assert(evicted.size() == 1);

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    // the std::future is settled from the producer's worker thread.
    assert(kv_put(store, "doomed", payload, EntryKind::Volatile, 0));
    std::future<int64_t> pending = kv_compact_async(store);
    assert(pending.get() == 3);
    assert(kv_count(store) == 0);

    // No explicit kv_close_store: the producer frees the store there *and* the
    // Store destructor calls Store_destroy, so closing here would double-free.
    // RAII handles teardown when `store` leaves scope.
    std::printf("cpp/kvstore: OK\n");
    return 0;
}
