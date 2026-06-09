// Conformance consumer: kvstore sample, C++ target.
//
// Drives the generated header-only wrappers, focusing on the map return-by-value
// redesign: `Entry::metadata()` reads the triple-pointer out-params into an
// `unordered_map`. Also covers the `[string]` list getter, the builder's
// list/map *input* marshaling, the iterator-backed `kv_list_keys`, and the
// `kv.stats` submodule wrapper. Aborts (non-zero) on any failed assertion.

#include <cassert>
#include <cstdio>
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

    // No explicit kv_close_store: the producer frees the store there *and* the
    // Store destructor calls Store_destroy, so closing here would double-free.
    // RAII handles teardown when `store` leaves scope.
    std::printf("cpp/kvstore: OK\n");
    return 0;
}
