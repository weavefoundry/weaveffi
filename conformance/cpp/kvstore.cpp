// Conformance consumer: kvstore sample, C++ target (ABI revision 2).
//
// Drives the generated header-only wrapper over the object-graph surface:
//  - `Store` is a copyable RAII interface wrapper (copies `_clone`, moves
//    transfer, destructors release) constructed via the `Store::open`
//    static factory, which throws the typed `IoError` on failure.
//  - `share()` returns a wrapper to the SAME producer object; `fork()` a new
//    one; `larger(std::optional<Store>)` exercises `Store?` both ways;
//    `describe()` returns a record carrying a `Store` field and an optional
//    one; `open_many` returns a vector of objects; `total_count` takes a
//    vector of objects plus an optional record holding an object.
//  - `EvictionListener` is an abstract class the consumer subclasses; a
//    returned `false` detaches (and frees) it, replacing or clearing frees
//    the previous one, and an implementation that throws surfaces to the
//    caller as `WeaveFFIError` code -4.
//  - the pre-existing surface still works: value records with optional,
//    list, map, and bytes fields, the `KvError` hierarchy, the lazy
//    `list_keys` range, the `kv::stats` nested module, and the
//    std::future-backed cancellable `compact`.
// Exits non-zero on the first failed check.

#include <cstdio>
#include <cstdlib>
#include <future>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "weaveffi.hpp"

using namespace kvstore;

static void check(bool ok, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "cpp/kvstore: FAIL: %s\n", what);
        std::exit(1);
    }
}

// State outlives the listener so the test can inspect it after the producer
// has freed the implementation.
struct ListenerState {
    std::vector<std::pair<std::string, EvictionReason>> evictions;
    std::vector<int64_t> ids;
    int freed = 0;
};

class RecordingListener : public EvictionListener {
    std::shared_ptr<ListenerState> state_;
    size_t keep_while_fewer_than_;
    std::string bomb_key_;

public:
    RecordingListener(std::shared_ptr<ListenerState> state, size_t keep_while_fewer_than,
                      std::string bomb_key = "")
        : state_(std::move(state)),
          keep_while_fewer_than_(keep_while_fewer_than),
          bomb_key_(std::move(bomb_key)) {}

    ~RecordingListener() override { state_->freed++; }

    bool on_evict(const Entry& entry, EvictionReason reason) override {
        if (entry.key == bomb_key_) throw std::runtime_error("listener refused " + entry.key);
        state_->evictions.emplace_back(entry.key, reason);
        state_->ids.push_back(entry.id);
        return state_->evictions.size() < keep_while_fewer_than_;
    }
};

int main() {
    check_abi_version();

    check(Store::default_capacity() == 1000000, "static default_capacity");

    Store store = Store::open("/tmp/conformance-kvstore-cpp");
    check(store.handle() != nullptr, "open yields a live handle");

    // A failing constructor throws the most-derived typed exception.
    {
        bool caught = false;
        try {
            Store bad = Store::open("");
        } catch (const IoError& e) {
            caught = (e.code() == 1004);
            check(dynamic_cast<const KvError*>(&e) != nullptr, "IoError is a KvError");
            check(dynamic_cast<const WeaveFFIError*>(&e) != nullptr, "IoError is a WeaveFFIError");
        }
        check(caught, "open(\"\") throws IoError 1004");
    }

    const std::vector<uint8_t> payload{1, 2, 3};
    check(store.put("alpha", payload, EntryKind::Persistent, std::nullopt), "put alpha");
    check(store.put("beta", payload, EntryKind::Volatile, std::nullopt), "put beta");
    check(store.count() == 2, "count is 2");

    // Copy semantics: copies share the object; mutations through one are
    // visible through the other; destructors release without killing it.
    {
        Store copy = store;
        check(copy.handle() == store.handle(), "copy constructor clones the same object");
        check(copy.count() == 2, "copy is usable");
        check(copy.put("gamma", payload, EntryKind::Volatile, std::nullopt), "put through copy");
        check(store.count() == 3, "mutation through the copy is visible through the original");

        Store other = Store::open("/tmp/other");
        check(other.handle() != store.handle(), "a second open is a distinct object");
        other = store;
        check(other.handle() == store.handle(), "copy assignment clones the same object");
        Store& same = other;
        other = same;
        check(other.handle() == store.handle(), "self-assignment keeps the handle");

        Store moved = std::move(copy);
        check(moved.handle() == store.handle(), "move constructor transfers the handle");
        check(copy.handle() == nullptr, "moved-from wrapper is empty");
        Store moved_into = Store::open("/tmp/x");
        moved_into = std::move(other);
        check(moved_into.handle() == store.handle() && other.handle() == nullptr,
              "move assignment transfers the handle");
        check(store.delete_("gamma"), "delete gamma through the original");
        // copy (empty), other (empty), moved, moved_into all destruct here.
    }
    check(store.count() == 2, "store survives its copies' destructors");

    // share(): the same object again (refcount bump), not a snapshot.
    {
        Store shared = store.share();
        check(shared.handle() == store.handle(), "share returns the same object");
        check(shared.put("via-share", payload, EntryKind::Volatile, std::nullopt),
              "put through share");
        check(store.count() == 3, "mutation through share is visible through the original");
        check(store.delete_("via-share"), "delete via original");
        check(shared.count() == 2, "deletion visible through share");
    }

    // fork(): a distinct object with a copy of the entries.
    {
        Store forked = store.fork();
        check(forked.handle() != store.handle(), "fork is a distinct object");
        check(forked.count() == 2, "fork copied the live entries");
        check(forked.put("only-in-fork", payload, EntryKind::Volatile, std::nullopt),
              "put into fork");
        check(forked.count() == 3 && store.count() == 2, "fork is independent");
    }

    // larger(): `Store?` as a parameter and as a return.
    {
        Store empty = Store::open("/tmp/empty");
        check(!empty.larger(std::nullopt).has_value(), "larger(null) on an empty store is none");
        std::optional<Store> own = store.larger(std::nullopt);
        check(own.has_value() && own->handle() == store.handle(),
              "larger(null) on a non-empty store returns itself");
        std::optional<Store> bigger = empty.larger(store);
        check(bigger.has_value() && bigger->handle() == store.handle(),
              "larger(other) picks the bigger other");
        std::optional<Store> self_wins = store.larger(empty);
        check(self_wins.has_value() && self_wins->handle() == store.handle(),
              "larger(smaller) returns self");
        std::optional<Store> none;
        check(!empty.larger(none).has_value(), "disengaged optional passes as null");
    }

    // describe(): a record carrying the object itself plus an optional one.
    {
        StoreInfo info = store.describe("primary", std::nullopt);
        check(info.label == "primary", "describe label");
        check(info.count == 2, "describe count");
        check(info.store.handle() == store.handle(), "describe().store is the same object");
        check(!info.mirror.has_value(), "describe mirror absent");
        check(info.store.count() == 2, "the record's store is usable");

        Store mirror = Store::open("/tmp/mirror");
        StoreInfo with_mirror = store.describe("mirrored", mirror);
        check(with_mirror.mirror.has_value() && with_mirror.mirror->handle() == mirror.handle(),
              "describe mirror present and identical");

        // A record copy clones its object fields.
        StoreInfo info_copy = info;
        check(info_copy.store.handle() == store.handle(), "copied record shares the object");
    }

    // open_many(): a list of objects as a return; total_count(): a list of
    // objects and an optional record holding an object as parameters.
    {
        std::vector<Store> many = Store::open_many({"/a", "/b", "/c"});
        check(many.size() == 3, "open_many returns three stores");
        check(many[0].handle() != many[1].handle() && many[1].handle() != many[2].handle(),
              "open_many stores are distinct");
        check(many[0].put("m0", payload, EntryKind::Volatile, std::nullopt), "put into many[0]");
        check(many[0].put("m1", payload, EntryKind::Volatile, std::nullopt), "put into many[0]");
        check(many[2].put("m2", payload, EntryKind::Volatile, std::nullopt), "put into many[2]");
        check(many[0].count() == 2 && many[1].count() == 0 && many[2].count() == 1,
              "open_many stores are independent");

        check(Store::total_count(many, std::nullopt) == 3, "total_count over the list");
        StoreInfo info = store.describe("extra", std::nullopt);
        check(Store::total_count(many, info) == 5, "total_count adds the record's store");
        check(Store::total_count({}, info) == 2, "total_count with an empty list");
        check(Store::total_count({}, std::nullopt) == 0, "total_count with nothing");
        // Encoding the parameters cloned each object; every wrapper is still
        // valid and holds its own reference.
        check(many[0].count() == 2 && info.store.count() == 2,
              "wrappers survive being encoded into parameter buffers");

        bool caught = false;
        try {
            Store::open_many({"/ok", ""});
        } catch (const IoError& e) {
            caught = (e.code() == 1004);
        }
        check(caught, "open_many with an empty path throws IoError");
    }

    // Lazy iterator: sorted keys, one producer `next` per step.
    {
        std::vector<std::string> keys;
        for (auto&& k : store.list_keys(std::nullopt)) keys.push_back(k);
        check(keys.size() == 2 && keys[0] == "alpha" && keys[1] == "beta",
              "list_keys yields sorted keys");
        keys.clear();
        for (auto&& k : store.list_keys(std::string("al"))) keys.push_back(k);
        check(keys.size() == 1 && keys[0] == "alpha", "list_keys honors the prefix");
        auto range = store.list_keys(std::nullopt);
        auto it = range.begin();
        check(*it == "alpha", "early-abandoned range yields the first key");
    }

    // Value record with optional, list, map, and bytes fields.
    {
        std::optional<Entry> found = store.get("alpha");
        check(found.has_value(), "get alpha present");
        check(found->id > 0 && found->key == "alpha" && found->value == payload,
              "entry fields");
        check(found->created_at > 0 && !found->expires_at.has_value(), "entry timestamps");
        check(found->tags.empty() && found->metadata.empty(), "entry collections empty");

        check(store.put("ttl", payload, EntryKind::Volatile, 3600), "put with ttl");
        std::optional<Entry> ttl_entry = store.get("ttl");
        check(ttl_entry.has_value() && ttl_entry->expires_at.has_value() &&
                  *ttl_entry->expires_at > ttl_entry->created_at,
              "ttl surfaces as a present optional");
        check(store.delete_("ttl"), "delete ttl");
        check(!store.delete_("ttl"), "second delete reports false");

        // Drive the generated pack/unpack pair directly for the list and map
        // fields the sample never accepts as a parameter.
        Entry entry{7, "k", payload, 1000, 55, {"hot", "fast"}, {{"source", "test"}, {"env", "prod"}}};
        detail::BufferWriter w;
        detail::write_Entry(w, entry);
        detail::BufferReader r(w.data(), w.size());
        Entry back = detail::read_Entry(r);
        r.expect_end();
        check(back.id == 7 && back.key == "k" && back.value == payload && back.created_at == 1000,
              "entry scalar fields round-trip");
        check(back.expires_at.has_value() && *back.expires_at == 55, "entry optional round-trips");
        check(back.tags.size() == 2 && back.tags[0] == "hot" && back.tags[1] == "fast",
              "entry list round-trips in order");
        check(back.metadata.size() == 2 && back.metadata.at("source") == "test" &&
                  back.metadata.at("env") == "prod",
              "entry map round-trips");
    }

    // Typed errors: the per-code subclass is the most-derived type.
    {
        bool caught_base = false;
        try {
            store.get("missing");
        } catch (const KvError& e) {
            caught_base = (e.code() == 1001);
        }
        check(caught_base, "missing key throws KvError 1001");
        bool caught_typed = false;
        try {
            store.get("missing");
        } catch (const KeyNotFoundError& e) {
            caught_typed = (std::string(e.what()) == "key not found");
        }
        check(caught_typed, "missing key throws KeyNotFoundError with the doc message");
    }

    // kv.stats nested module takes the interface by const reference.
    {
        Stats st = kv::stats::get_stats(store);
        check(st.total_entries == 2 && st.total_bytes == 6 && st.expired_entries == 0,
              "get_stats snapshot");
    }

    // Eviction listener: delete and expiry-on-read notify it; returning
    // false detaches it (the producer frees it); replacement and clear free.
    {
        auto state = std::make_shared<ListenerState>();
        store.set_eviction_listener(std::make_shared<RecordingListener>(state, 2));
        check(state->freed == 0, "attached listener is retained");

        check(store.put("evict-me", payload, EntryKind::Volatile, std::nullopt), "put evict-me");
        std::optional<Entry> to_evict = store.get("evict-me");
        check(store.delete_("evict-me"), "delete evict-me");
        check(state->evictions.size() == 1 && state->evictions[0].first == "evict-me" &&
                  state->evictions[0].second == EvictionReason::Deleted,
              "delete notified the listener with Deleted");
        check(to_evict.has_value() && state->ids[0] == to_evict->id,
              "the evicted entry carries its id");

        check(store.put("expiring", payload, EntryKind::Volatile, -1), "put already expired");
        bool caught = false;
        try {
            store.get("expiring");
        } catch (const ExpiredError& e) {
            caught = (e.code() == 1002);
        }
        check(caught, "reading an expired entry throws ExpiredError 1002");
        check(state->evictions.size() == 2 && state->evictions[1].first == "expiring" &&
                  state->evictions[1].second == EvictionReason::Expired,
              "expiry on read notified the listener with Expired");
        // The second eviction returned false, so the store detached and
        // freed the listener; a third eviction is not observed.
        check(state->freed == 1, "detached listener was freed");
        check(store.put("again", payload, EntryKind::Volatile, std::nullopt), "put again");
        check(store.delete_("again"), "delete again");
        check(state->evictions.size() == 2, "detached listener is not notified");

        auto first = std::make_shared<ListenerState>();
        auto second = std::make_shared<ListenerState>();
        store.set_eviction_listener(std::make_shared<RecordingListener>(first, 1000));
        check(first->freed == 0, "first listener retained");
        store.set_eviction_listener(std::make_shared<RecordingListener>(second, 1000));
        check(first->freed == 1, "replaced listener is freed");
        check(second->freed == 0, "replacement is retained");
        store.clear_eviction_listener();
        check(second->freed == 1, "cleared listener is freed");
        store.clear_eviction_listener();

        bool threw = false;
        try {
            store.set_eviction_listener(nullptr);
        } catch (const std::invalid_argument&) {
            threw = true;
        }
        check(threw, "null listener throws std::invalid_argument");
    }

    // A listener that throws surfaces to the caller as a foreign error (-4)
    // on a `throws` method: the generic WeaveFFIError, not a KvError.
    {
        auto state = std::make_shared<ListenerState>();
        store.set_eviction_listener(std::make_shared<RecordingListener>(state, 1000, "bomb"));
        check(store.put("bomb", payload, EntryKind::Volatile, std::nullopt), "put bomb");
        bool caught = false;
        try {
            store.delete_("bomb");
        } catch (const KvError&) {
            check(false, "a foreign error must not be mapped to a domain exception");
        } catch (const WeaveFFIError& e) {
            caught = true;
            check(e.code() == -4, "listener exception maps to FOREIGN_ERROR_CODE (-4)");
            check(std::string(e.what()).find("listener refused bomb") != std::string::npos,
                  "foreign error carries the C++ exception message");
        }
        check(caught, "delete threw for a throwing listener");
        check(!store.delete_("bomb"), "the entry was removed before the listener ran");
        check(state->freed == 0, "a throwing listener stays attached");
        // Still usable, and a non-bomb key is observed normally.
        check(store.put("fine", payload, EntryKind::Volatile, std::nullopt), "put fine");
        check(store.delete_("fine"), "delete fine");
        check(state->evictions.size() == 1 && state->evictions[0].first == "fine",
              "listener still attached after the foreign error");
        store.clear_eviction_listener();
        check(state->freed == 1, "listener freed on clear");
    }

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim.
    {
        check(store.put("doomed", payload, EntryKind::Volatile, 0), "put doomed");
        std::future<int64_t> pending = store.compact();
        check(pending.get() == 3, "compact reclaimed the expired bytes");
        check(store.count() == 2, "compact left the live entries");

        weaveffi_cancel_token* token = weaveffi_cancel_token_create();
        weaveffi_cancel_token_cancel(token);
        std::future<int64_t> cancelled = store.compact(token);
        bool caught = false;
        try {
            cancelled.get();
        } catch (const IoError& e) {
            caught = (e.code() == 1004);
        }
        check(caught, "a pre-cancelled compact settles the future with IoError");
        weaveffi_cancel_token_destroy(token);
    }

    // Non-throwing method still works, then release everything via RAII.
    store.clear();
    check(store.count() == 0, "clear empties the store");

    // Destroying the last wrapper releases the object; a move-emptied wrapper
    // destructs as a no-op.
    {
        Store last = std::move(store);
        check(store.handle() == nullptr, "moved-from store is empty");
        check(last.count() == 0, "moved-to store is usable");
    }

    std::printf("cpp/kvstore: OK\n");
    return 0;
}
