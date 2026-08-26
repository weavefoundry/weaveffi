// Conformance consumer: kvstore sample, C++ target.
//
// Drives the generated header-only wrappers for the value-type surface +
// typed error surface: `Store` is a move-only RAII interface class
// constructed via the `Store::open` static factory (destroy runs in the
// destructor) and `Store::default_capacity` is a static member, while `Entry`
// and `Stats` are plain value structs decoded from value buffers (with the
// optional, list, and map fields riding inside the same buffer). Throwing
// wrappers surface the `KvError` domain hierarchy (`KeyNotFoundError`,
// `IoError`, ...), including from the std::future-backed async `compact`.
// Also keeps the pre-overhaul coverage: list/map fields round-tripping
// through the generated pack/unpack pair, the iterator-backed `list_keys`,
// the `kv.stats` nested-module wrapper, and the std::function eviction
// listener. Aborts (non-zero) on any failed assertion.

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
    // Static member on the interface class.
    assert(Store::default_capacity() == 1000000);

    // Constructor `open` is not named `new`, so it maps to a static factory.
    Store store = Store::open("/tmp/conformance-kvstore-cpp");

    // A failing constructor throws the typed domain exception.
    bool caught_open = false;
    try {
        Store bad = Store::open("");
    } catch (const IoError& e) {
        caught_open = (e.code() == 1004);
    }
    assert(caught_open);

    const std::vector<uint8_t> payload{1, 2, 3};
    assert(store.put("alpha", payload, EntryKind::Persistent, std::nullopt));
    assert(store.put("beta", payload, EntryKind::Volatile, std::nullopt));
    assert(store.count() == 2);

    // Iterator-backed listing: a lazy single-pass range, drained into a
    // vector here (one producer `next` per step).
    std::vector<std::string> keys;
    for (auto&& k : store.list_keys(std::nullopt)) keys.push_back(k);
    assert(keys.size() == 2);
    bool saw_alpha = false, saw_beta = false;
    for (const auto& k : keys) {
        if (k == "alpha") saw_alpha = true;
        if (k == "beta") saw_beta = true;
    }
    assert(saw_alpha && saw_beta);

    // A present key comes back as an engaged optional holding the decoded
    // value struct; the optional, list, and map fields decode with it (empty
    // here: the producer stores no TTL, tags, or metadata on put).
    std::optional<Entry> found = store.get("alpha");
    assert(found.has_value());
    assert(found->id > 0);
    assert(found->key == "alpha");
    assert(found->value == payload);
    assert(found->created_at > 0);
    assert(!found->expires_at.has_value());
    assert(found->tags.empty());
    assert(found->metadata.empty());

    // A put with a TTL surfaces as a present optional field on read.
    assert(store.put("ttl", payload, EntryKind::Volatile, 3600));
    std::optional<Entry> ttl_entry = store.get("ttl");
    assert(ttl_entry.has_value());
    assert(ttl_entry->expires_at.has_value());
    assert(*ttl_entry->expires_at > ttl_entry->created_at);
    assert(store.delete_("ttl"));

    // A missing key throws the per-code subclass; catching the domain base
    // proves the hierarchy (KeyNotFoundError -> KvError -> WeaveFFIError).
    bool caught_missing = false;
    try {
        store.get("missing");
    } catch (const KvError& e) {
        caught_missing = (e.code() == 1001);
    }
    assert(caught_missing);

    // The most-derived type is the per-code class.
    bool caught_typed = false;
    try {
        store.get("missing");
    } catch (const KeyNotFoundError&) {
        caught_typed = true;
    }
    assert(caught_typed);

    // The sample declares no Entry-typed parameter, so drive the generated
    // pack/unpack pair directly: a [string] list and a {string:string} map
    // encode *in* and decode back *out* of one value buffer (the coverage the
    // builder + getters used to give).
    Entry entry{7,
                "alpha",
                payload,
                1000,
                std::nullopt,
                {"hot", "fast"},
                {{"source", "test"}, {"env", "prod"}}};
    detail::BufferWriter w;
    detail::write_Entry(w, entry);
    detail::BufferReader r(w.data(), w.size());
    Entry back = detail::read_Entry(r);
    r.expect_end();
    assert(back.id == 7);
    assert(back.key == "alpha");
    assert(back.value == payload);
    assert(back.created_at == 1000);
    assert(!back.expires_at.has_value());

    assert(back.tags.size() == 2);
    bool saw_hot = false, saw_fast = false;
    for (const auto& t : back.tags) {
        if (t == "hot") saw_hot = true;
        if (t == "fast") saw_fast = true;
    }
    assert(saw_hot && saw_fast);

    assert(back.metadata.size() == 2);
    assert(back.metadata.at("source") == "test");
    assert(back.metadata.at("env") == "prod");

    // Empty list and map fields round-trip as empty (the zero-count branch).
    Entry empty{8, "k", payload, 1, std::nullopt, {}, {}};
    detail::BufferWriter we;
    detail::write_Entry(we, empty);
    detail::BufferReader re(we.data(), we.size());
    Entry empty_back = detail::read_Entry(re);
    re.expect_end();
    assert(empty_back.tags.empty());
    assert(empty_back.metadata.empty());

    // kv.stats nested-module wrapper takes the interface by const reference
    // and returns the Stats value struct.
    Stats st = kv::stats::get_stats(store);
    assert(st.total_entries == 2);
    assert(st.total_bytes == 6);

    // Eviction listener: delete fires the std::function trampoline
    // synchronously on the calling thread. `delete` is a C++ keyword, so the
    // method is escaped to `delete_`.
    std::vector<std::string> evicted;
    uint64_t sub = kv::register_eviction_listener(
        [&evicted](std::string key) { evicted.push_back(std::move(key)); });
    assert(sub > 0);
    assert(store.delete_("beta"));
    assert(evicted.size() == 1 && evicted[0] == "beta");

    // Unregister stops delivery.
    kv::unregister_eviction_listener(sub);
    assert(store.delete_("alpha"));
    assert(evicted.size() == 1);

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    // the std::future is settled from the producer's worker thread.
    assert(store.put("doomed", payload, EntryKind::Volatile, 0));
    std::future<int64_t> pending = store.compact();
    assert(pending.get() == 3);
    assert(store.count() == 0);

    // A pre-cancelled compact settles the future with the typed exception.
    weaveffi_cancel_token* token = weaveffi_cancel_token_create();
    weaveffi_cancel_token_cancel(token);
    std::future<int64_t> cancelled = store.compact(token);
    bool caught_async = false;
    try {
        cancelled.get();
    } catch (const IoError& e) {
        caught_async = (e.code() == 1004);
    }
    assert(caught_async);
    weaveffi_cancel_token_destroy(token);

    // Non-throwing method (clear declares no error domain) still works.
    assert(store.put("last", payload, EntryKind::Volatile, std::nullopt));
    store.clear();
    assert(store.count() == 0);

    // RAII: the Store destructor calls weaveffi_kv_Store_destroy when `store`
    // leaves scope; no explicit close.
    std::printf("cpp/kvstore: OK\n");
    return 0;
}
