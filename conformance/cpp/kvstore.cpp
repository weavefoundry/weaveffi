// Conformance consumer: kvstore sample, C++ target (ABI revision 5).
//
// Drives the feature-complete producer through the generated header:
//
//   * the load-time check (ABI revision and both modules' contract tables;
//     a doctored table is refused by name);
//   * the `Store` RAII class: fallible and infallible constructors, methods,
//     statics, the deprecated `size`, records (`Entry`, `StoreInfo`), the
//     `EntryKind` enum, maps and optionals, and the logical clock;
//   * the `KvError` hierarchy with payload fields (`KeyNotFoundError`,
//     `ExpiredError`, `StoreFullError`, `RejectedError`,
//     `CallbackFailedError`) and runtime codes on throwing calls (the root
//     `Error`);
//   * lazy `Range`s of strings (throwing), records, objects, and optional
//     scalars;
//   * four callback interfaces implemented as subclasses: a `Listener`
//     (retained, filtered by `accepts`, told about every `Change`, detached
//     when it throws, and notified from a producer thread during
//     compaction), a `Policy` (an optional scalar parameter and return in
//     `ttl_for`, a record return, a typed `RejectedError` thrown back through
//     `put` with its fields, an object parameter and object return, and
//     failures the sample turns into `CallbackFailedError`), a `Loader`
//     passed as an optional callback (string, bytes, and optional-object
//     returns; typed errors decoded by the producer or passed through), and
//     a `Scorer` taking and returning typed arrays;
//   * `Store` objects in every position, compared by identity (`==`);
//   * futures: an async free function returning an object, a cancellable
//     method cancelled mid-pause, an async list, an async function in the
//     nested `kv.stats` module, and concurrent calls;
//   * the nested `kv.stats` module and the sibling `report` root;
//   * the ABI 5 shapes: an optional scalar return, iterator item, and async
//     result; typed arrays as a return and an async result; a `usize`
//     return; and a `throws: any` method failing with the root `Error` (-1).
//
// Ends by asserting every callback implementation was released and the
// producer's leak counters are all zero.

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <future>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "check.hpp"
#include "kvstore.hpp"

using namespace kvstore;
namespace stats = ::kvstore::kv::stats;

static const std::thread::id g_main_thread = std::this_thread::get_id();
static std::atomic<int> g_listeners_freed{0};
static std::atomic<int> g_policies_freed{0};
static std::atomic<int> g_loaders_freed{0};
static std::atomic<int> g_scorers_freed{0};

static std::vector<uint8_t> bytes(std::string_view s) { return std::vector<uint8_t>(s.begin(), s.end()); }

static bool starts_with(std::string_view s, std::string_view prefix) {
    return s.substr(0, prefix.size()) == prefix;
}

static Entry put(const Store& s, std::string_view key, std::string_view value,
                 EntryKind kind = EntryKind::Persistent, std::optional<int64_t> ttl = std::nullopt) {
    return s.put(key, bytes(value), kind, ttl);
}

template <typename F>
static KeyNotFoundError expect_key_not_found(F&& fn, const std::string& key, const char* what) {
    KeyNotFoundError e = expect_throw<KeyNotFoundError>(std::forward<F>(fn), what);
    CHECK(e.code() == 1001 && e.key == key);
    return e;
}

// ── load ────────────────────────────────────────────────────────────────────

static void load_checks() {
    check_library();

    // A table entry the library lacks, or one whose signature changed, is
    // refused by its path.
    const detail::ContractEntry missing[] = {{1, 2, "kv.Store.vanished"}};
    CHECK(detail::contract_mismatch(kvstore_kv_contract, missing) ==
          "kv.Store.vanished is missing from the library");
    const detail::ContractEntry& first = detail::kv_contract[0];
    const detail::ContractEntry changed[] = {{first.id, first.hash ^ 1, first.path}};
    CHECK(detail::contract_mismatch(kvstore_kv_contract, changed) ==
          std::string(first.path) + " changed since these bindings were generated");
    CHECK(detail::contract_mismatch(kvstore_kv_contract, detail::kv_contract).empty());
    CHECK(detail::contract_mismatch(kvstore_report_contract, detail::report_contract).empty());
}

// ── constructors ────────────────────────────────────────────────────────────

static void constructors() {
    InvalidPathError e = expect_throw<InvalidPathError>([] { Store::open(""); }, "open('')");
    CHECK(e.code() == 1004 && std::string(e.what()) == "invalid path");
    CHECK(dynamic_cast<const KvError*>(&e) != nullptr && dynamic_cast<const Error*>(&e) != nullptr);

    Store s;
    CHECK(s.path() == "memory");
    CHECK(s.capacity() == Store::default_capacity() && s.capacity() == 1000000);

    Store opened = kv::open_store("/async").get();
    CHECK(opened.path() == "/async");
    std::future<Store> failing = kv::open_store("");
    e = expect_throw<InvalidPathError>([&] { failing.get(); }, "open_store('')");
    CHECK(e.code() == 1004 && std::string(e.what()) == "invalid path");
}

// ── basics ──────────────────────────────────────────────────────────────────

static void basics() {
    Store s = Store::open("/basics");
    Entry e = put(s, "alpha", "one");
    CHECK(e.key == "alpha" && e.value == bytes("one") && e.kind == EntryKind::Persistent);
    CHECK(e.version == 1 && !e.expires_at.has_value() && e.tags.empty() && e.metadata.empty());
    e = put(s, "alpha", "two", EntryKind::Volatile);
    CHECK(e.version == 2 && e.kind == EntryKind::Volatile);
    CHECK(s.get("alpha").value == bytes("two"));
    KeyNotFoundError nf = expect_key_not_found([&] { s.get("nope"); }, "nope", "get('nope')");
    CHECK(std::string(nf.what()) == "key not found: nope");
    std::optional<Entry> found = s.find("alpha");
    CHECK(found.has_value() && found->version == 2);
    CHECK(!s.find("nope").has_value());

    // TTLs follow the logical clock; an expired get reports when.
    CHECK(put(s, "ttl", "x", EntryKind::Volatile, 10).expires_at == 10);
    CHECK(s.now() == 0);
    CHECK(s.tick(9) == 9 && s.count() == 2);
    CHECK(s.tick(1) == 10 && s.count() == 1);
    ExpiredError ex = expect_throw<ExpiredError>([&] { s.get("ttl"); }, "get expired");
    CHECK(ex.code() == 1002 && ex.key == "ttl" && ex.expired_at == 10);
    expect_key_not_found([&] { s.get("ttl"); }, "ttl", "the expired read removed it");

    // Capacity: a new key past it is StoreFull { capacity }.
    s.set_capacity(1);
    CHECK(s.capacity() == 1);
    put(s, "alpha", "three");
    StoreFullError full = expect_throw<StoreFullError>([&] { put(s, "beta", "b"); }, "StoreFull");
    CHECK(full.code() == 1003 && full.capacity == 1);
    s.set_capacity(100);

    // An undeclared enum value is a marshalling failure: on a call that
    // declares errors it's the root Error, not a KvError.
    Error bad = expect_throw<Error>(
        [&] { s.put("k", bytes("v"), static_cast<EntryKind>(9), std::nullopt); }, "bad EntryKind");
    CHECK(bad.code() == -3 && dynamic_cast<const KvError*>(&bad) == nullptr);

    put(s, "beta", "b");
    CHECK(s.delete_("beta") && !s.delete_("beta"));
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    CHECK(s.size() == s.count() && s.count() == 1);
#pragma GCC diagnostic pop
    CHECK(s.clear() == 1 && s.count() == 0);
}

// ── iterators ───────────────────────────────────────────────────────────────

static void iterators() {
    Store s = Store::open("/iter");
    put(s, "user.bob", "b");
    put(s, "user.alice", "a");
    put(s, "sys.x", "xx");

    std::vector<std::string> keys;
    for (const std::string& k : s.keys(std::nullopt)) keys.push_back(k);
    CHECK((keys == std::vector<std::string>{"sys.x", "user.alice", "user.bob"}));
    expect_key_not_found([&] { s.keys(std::string("zzz")); }, "zzz", "keys('zzz')");

    // Abandoning a range part-way releases the producer iterator.
    {
        auto range = s.keys(std::string("user."));
        std::optional<std::string> next = range.next();
        CHECK(next == "user.alice");
        CHECK(kvstore_debug_live(2) == 1);
    }
    CHECK(kvstore_debug_live(2) == 0);

    std::vector<Entry> entries;
    for (Entry& e : s.entries(std::string("sys."))) entries.push_back(std::move(e));
    CHECK(entries.size() == 1 && entries[0].key == "sys.x" && entries[0].value == bytes("xx"));

    const std::vector<std::string> prefixes{"user.", "sys.", "none."};
    std::vector<Store> parts;
    for (Store& p : s.partition(prefixes)) parts.push_back(std::move(p));
    CHECK(parts.size() == 3);
    const uint32_t counts[] = {2, 1, 0};
    for (size_t i = 0; i < parts.size(); ++i) {
        CHECK(parts[i] != s);
        for (size_t j = 0; j < i; ++j) CHECK(parts[i] != parts[j]);
        CHECK(parts[i].count() == counts[i] && parts[i].path() == prefixes[i]);
    }
}

// ── listener ────────────────────────────────────────────────────────────────

// Records every change; `accepts` says no to `skip` and throws on `fail_on`.
class RecordingListener : public Listener {
    std::string skip_;
    std::string fail_on_;
    mutable std::mutex mu_;
    std::vector<Change> changes_;
    std::vector<std::thread::id> threads_;

public:
    explicit RecordingListener(std::string skip = "", std::string fail_on = "")
        : skip_(std::move(skip)), fail_on_(std::move(fail_on)) {}

    ~RecordingListener() override { ++g_listeners_freed; }

    bool accepts(std::string_view key) override {
        if (!fail_on_.empty() && key == fail_on_) throw std::runtime_error("listener refused");
        return key != skip_;
    }

    void on_change(const Change& change) override {
        std::lock_guard<std::mutex> lock(mu_);
        threads_.push_back(std::this_thread::get_id());
        changes_.push_back(change);
    }

    std::vector<Change> changes() const {
        std::lock_guard<std::mutex> lock(mu_);
        return changes_;
    }

    std::vector<std::thread::id> threads() const {
        std::lock_guard<std::mutex> lock(mu_);
        return threads_;
    }

    Change last() const {
        std::lock_guard<std::mutex> lock(mu_);
        CHECK(!changes_.empty());
        return changes_.back();
    }

    template <typename V>
    size_t count_of() const {
        std::lock_guard<std::mutex> lock(mu_);
        size_t n = 0;
        for (const Change& c : changes_) n += std::holds_alternative<V>(c.value) ? 1 : 0;
        return n;
    }
};

static void listeners() {
    Store s = Store::open("/listen");
    auto listener = std::make_shared<RecordingListener>("quiet");
    uint32_t sub = s.subscribe(listener);
    CHECK(sub > 0 && s.listener_count() == 1 && listener.use_count() == 2);

    put(s, "a", "1");
    Change last = listener->last();
    const auto* p = std::get_if<Change::Put>(&last.value);
    CHECK(p != nullptr && p->entry.version == 1 && !p->replaced);
    put(s, "a", "2");
    last = listener->last();
    p = std::get_if<Change::Put>(&last.value);
    CHECK(p != nullptr && p->entry.version == 2 && p->replaced);
    put(s, "quiet", "x");
    CHECK(listener->count_of<Change::Put>() == 2);
    CHECK(s.delete_("a"));
    last = listener->last();
    const auto* r = std::get_if<Change::Removed>(&last.value);
    CHECK(r != nullptr && r->key == "a" && !r->expired);

    // An expired read removes the entry and says so.
    put(s, "short", "x", EntryKind::Volatile, 1);
    s.tick(1);
    expect_throw<ExpiredError>([&] { s.get("short"); }, "get short");
    last = listener->last();
    r = std::get_if<Change::Removed>(&last.value);
    CHECK(r != nullptr && r->key == "short" && r->expired);
    CHECK(s.clear() == 1);
    last = listener->last();
    CHECK(last.tag() == Change::Tag::Cleared && std::get<Change::Cleared>(last.value).count == 1);
    for (std::thread::id t : listener->threads()) CHECK(t == g_main_thread);

    // Unsubscribing releases the producer's reference exactly once.
    CHECK(s.unsubscribe(sub) && listener.use_count() == 1);
    CHECK(!s.unsubscribe(sub) && s.listener_count() == 0);
    int freed = g_listeners_freed;
    listener.reset();
    CHECK(g_listeners_freed == freed + 1);

    // A listener that throws is detached (and released); the put succeeds.
    auto failing = std::make_shared<RecordingListener>("", "boom");
    s.subscribe(failing);
    put(s, "fine", "1");
    CHECK(failing->count_of<Change::Put>() == 1);
    put(s, "boom", "1");
    CHECK(s.count() == 2 && s.listener_count() == 0 && failing.use_count() == 1);

    // A null listener is refused before the call, as a marshalling failure.
    Error null_listener = expect_throw<Error>([&] { s.subscribe(nullptr); }, "subscribe(nullptr)");
    CHECK(null_listener.code() == -3);

    // Destroying the store releases the listeners it still holds.
    freed = g_listeners_freed;
    s.subscribe(std::make_shared<RecordingListener>());
    s.subscribe(std::make_shared<RecordingListener>());
    CHECK(s.listener_count() == 2 && g_listeners_freed == freed);
    { Store gone = std::move(s); }
    CHECK(g_listeners_freed == freed + 2);
}

// ── policy ──────────────────────────────────────────────────────────────────

// Tags and encrypts admitted entries, routes `b/` keys to `other`, and fails
// in various ways for some keys.
class TestPolicy : public Policy {
    Store other_;

public:
    std::atomic<int> admitted{0};
    std::atomic<bool> saw_version_zero{true};

    explicit TestPolicy(Store other) : other_(std::move(other)) {}
    ~TestPolicy() override { ++g_policies_freed; }

    // "short" lives one tick, "forever" never expires, "ttl-fail" fails with
    // a typed error, and every other key keeps the requested TTL.
    std::optional<int64_t> ttl_for(std::string_view key, std::optional<int64_t> requested) override {
        if (key == "short") return 1;
        if (key == "forever") return std::nullopt;
        if (key == "ttl-fail") throw InvalidPathError("no ttl for you");
        return requested;
    }

    Entry admit(const Entry& entry) override {
        ++admitted;
        if (entry.version != 0) saw_version_zero = false;
        if (starts_with(entry.key, "secret")) {
            throw RejectedError("secrets are not stored", entry.key, "no secrets");
        }
        if (starts_with(entry.key, "boom")) throw std::runtime_error("policy exploded");
        Entry out = entry;
        out.tags = {"admitted"};
        out.kind = EntryKind::Encrypted;
        out.key = "renamed";  // ignored by the store
        // An undeclared enum value makes the record malformed (-3).
        if (starts_with(entry.key, "garbage")) out.kind = static_cast<EntryKind>(9);
        return out;
    }

    Store route(std::string_view key, Store home) override {
        if (starts_with(key, "b/")) return other_;
        if (starts_with(key, "null/")) {
            // A moved-from wrapper is a null object, which `route` can't
            // return (-3).
            Store taken = std::move(home);
            return home;
        }
        return home;
    }
};

static void policies() {
    Store s = Store::open("/policy");
    Store other = Store::open("/other");
    auto policy = std::make_shared<TestPolicy>(other);
    s.set_policy(policy);
    CHECK(s.has_policy() && policy.use_count() == 2);

    // admit's record return is what's stored (its key and version aside).
    Entry e = put(s, "a", "1", EntryKind::Volatile);
    CHECK(e.key == "a" && e.version == 1 && e.kind == EntryKind::Encrypted);
    CHECK((e.tags == std::vector<std::string>{"admitted"}));

    // route: the object parameter and object return redirect a write.
    put(s, "b/x", "2", EntryKind::Volatile);
    CHECK(s.count() == 1 && other.count() == 1);

    // A typed error thrown by the throwing callback reaches the caller with
    // its code and fields, and the domain's own message for them.
    RejectedError rej = expect_throw<RejectedError>([&] { put(s, "secret", "3"); }, "Rejected");
    CHECK(rej.code() == 1005 && rej.key == "secret" && rej.reason == "no secrets");
    CHECK(std::string(rej.what()) == "write to secret rejected: no secrets");

    // Any other exception reaches the producer as a callback failure, which
    // the sample turns into CallbackFailed with the exception's message.
    CallbackFailedError boom =
        expect_throw<CallbackFailedError>([&] { put(s, "boom", "4"); }, "non-domain failure");
    CHECK(boom.code() == 1006 && boom.message == "policy exploded");
    CHECK(std::string(boom.what()) == "policy exploded");

    // So is a return the producer can't accept: a malformed record, or a
    // null required object.
    expect_throw<CallbackFailedError>([&] { put(s, "garbage", "5"); }, "malformed admit");
    expect_throw<CallbackFailedError>([&] { put(s, "null/x", "6"); }, "null route");
    CHECK(s.count() == 1 && other.count() == 1);
    CHECK(policy->admitted == 6 && policy->saw_version_zero);

    // ttl_for: an optional scalar in and out, consulted before admit.
    CHECK(put(s, "short", "x", EntryKind::Volatile).expires_at == 1);
    CHECK(!put(s, "forever", "x", EntryKind::Volatile, 5).expires_at.has_value());
    CHECK(put(s, "kept", "x", EntryKind::Volatile, 5).expires_at == 5);
    InvalidPathError ttl = expect_throw<InvalidPathError>([&] { put(s, "ttl-fail", "x"); }, "ttl_for fails");
    CHECK(ttl.code() == 1004 && std::string(ttl.what()) == "invalid path");
    CHECK(s.count() == 4 && policy->admitted == 9);

    // Replacing the policy releases the old one; an empty pointer removes it.
    s.set_policy(std::make_shared<TestPolicy>(other));
    CHECK(policy.use_count() == 1);
    int freed = g_policies_freed;
    s.set_policy(nullptr);
    CHECK(g_policies_freed == freed + 1 && !s.has_policy());
    put(s, "secret", "now allowed");
    CHECK(s.count() == 5);
}

// ── loader ──────────────────────────────────────────────────────────────────

class TestLoader : public Loader {
    std::optional<Store> backup_;

public:
    explicit TestLoader(std::optional<Store> backup = std::nullopt) : backup_(std::move(backup)) {}
    ~TestLoader() override { ++g_loaders_freed; }

    std::string name() override { return "cpp-loader"; }

    std::optional<Store> fallback(std::string_view key) override {
        return key == "fb" ? backup_ : std::nullopt;
    }

    std::vector<uint8_t> load(std::string_view key) override {
        if (key == "missing") throw KeyNotFoundError("not in the loader", "missing");
        if (key == "elsewhere") throw KeyNotFoundError("not in the loader", "other");
        if (key == "broken") throw std::runtime_error("loader is broken");
        return bytes("loaded:" + std::string(key));
    }
};

static void loaders() {
    Store s = Store::open("/load");
    CHECK(!s.get_or_load("k", nullptr).has_value());

    auto loader = std::make_shared<TestLoader>();
    std::optional<Entry> e = s.get_or_load("k", loader);
    CHECK(e.has_value() && e->value == bytes("loaded:k") && e->kind == EntryKind::Volatile);
    CHECK(e->metadata.size() == 1 && e->metadata.at("source") == "cpp-loader");
    CHECK(loader.use_count() == 1);  // released before the call returned
    CHECK(s.count() == 1);
    e = s.get_or_load("k", loader);
    CHECK(e.has_value() && e->version == 1);

    {
        Store backup = Store::open("/backup");
        put(backup, "fb", "from backup");
        e = s.get_or_load("fb", std::make_shared<TestLoader>(backup));
        CHECK(e.has_value() && e->value == bytes("from backup") && e->kind == EntryKind::Persistent);
    }

    CHECK(!s.get_or_load("missing", loader).has_value());
    KeyNotFoundError nf =
        expect_key_not_found([&] { s.get_or_load("elsewhere", loader); }, "other", "elsewhere");
    CHECK(std::string(nf.what()) == "key not found: other");
    CallbackFailedError broken =
        expect_throw<CallbackFailedError>([&] { s.get_or_load("broken", loader); }, "broken loader");
    CHECK(broken.message == "loader is broken");
    CHECK(loader.use_count() == 1);
    int freed = g_loaders_freed;
    loader.reset();
    CHECK(g_loaders_freed == freed + 1);
}

// ── async ───────────────────────────────────────────────────────────────────

static void async_calls() {
    Store s = Store::open("/async-calls");
    auto listener = std::make_shared<RecordingListener>();
    s.subscribe(listener);
    put(s, "old1", "x", EntryKind::Volatile, 1);
    put(s, "old2", "x", EntryKind::Volatile, 1);
    put(s, "keep", "x");
    s.tick(5);

    // compact runs on a producer thread and notifies listeners there.
    {
        CancelToken token;
        CHECK(s.compact(0, token).get() == 2);
    }
    CHECK(listener->count_of<Change::Removed>() == 2);
    std::vector<std::thread::id> threads = listener->threads();
    CHECK(threads.size() == 5);  // three puts, then the two removals
    CHECK(threads[3] != g_main_thread && threads[4] != g_main_thread);
    CHECK(s.count() == 1);

    // No token never cancels.
    CHECK(s.compact(5).get() == 0);

    // Cancel mid-pause: the call stops at once, and the background pause
    // notices the token and stops.
    CancelToken token;
    std::future<uint32_t> pending = s.compact(60000, token);
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
    CHECK(pending.wait_for(std::chrono::seconds(0)) != std::future_status::ready);
    CHECK(Store::active_jobs() >= 1);
    auto started = std::chrono::steady_clock::now();
    token.cancel();
    expect_throw<Cancelled>([&] { pending.get(); }, "cancelled compact");
    CHECK(std::chrono::steady_clock::now() - started < std::chrono::seconds(1));
    for (int i = 0; i < 2000 && Store::active_jobs() != 0; ++i) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    CHECK(Store::active_jobs() == 0);

    // get_many: an async list of optional records, launched concurrently.
    std::vector<std::future<std::vector<std::optional<Entry>>>> many;
    for (int i = 0; i < 32; ++i) many.push_back(s.get_many({"keep", "gone", "keep"}));
    for (auto& f : many) {
        std::vector<std::optional<Entry>> got = f.get();
        CHECK(got.size() == 3 && got[0].has_value() && !got[1].has_value());
        CHECK(got[2].has_value() && got[2]->key == "keep");
    }

    // The nested module's async free function: objects in a list in, a
    // record out.
    Store other = Store::open("/other");
    put(other, "a", "123");
    Stats st = stats::summarize_all({s, other}).get();
    CHECK(st.entries == 2 && st.bytes == 4);
    CHECK(st.by_kind.size() == 1 && st.by_kind.at(EntryKind::Persistent) == 2);
}

// ── object graph ────────────────────────────────────────────────────────────

static void object_graph() {
    std::optional<Store> original = Store::open("/graph");
    put(*original, "k", "v");

    // share(): the same object behind a new wrapper; copies share it too.
    Store s = original->share();
    CHECK(s.handle() == original->handle());
    Store copy = s;
    CHECK(copy.handle() == s.handle());
    original.reset();
    CHECK(s.count() == 1 && copy.count() == 1);

    Store fork = s.fork();
    CHECK(fork.handle() != s.handle() && fork.count() == 1 && fork.path() == "/graph");
    put(fork, "k2", "v");
    CHECK(fork.count() == 2 && s.count() == 1);

    Store empty = Store::open("/empty");
    CHECK(!empty.larger(std::nullopt).has_value());
    std::optional<Store> larger = empty.larger(fork);
    CHECK(larger.has_value() && larger->handle() == fork.handle());
    larger = s.larger(std::nullopt);
    CHECK(larger.has_value() && larger->handle() == s.handle());

    StoreInfo info = s.describe("main", fork);
    CHECK(info.label == "main" && info.store == s && info.count == 1);
    CHECK(info.mirror == fork);
    CHECK(info == s.describe("main", fork) && info != s.describe("other", fork));
    CHECK(info.mirror->count() == 2);

    std::vector<Store> many = Store::open_many({"/a", "/b"});
    CHECK(many.size() == 2 && many[0].path() == "/a" && many[1].path() == "/b");
    expect_throw<InvalidPathError>([] { Store::open_many({"/a", ""}); }, "open_many with ''");

    std::unordered_map<std::string, Store> named =
        Store::by_label({info, StoreInfo{"first", many[0], std::nullopt, 0}});
    CHECK(named.size() == 2 && named.at("main").handle() == s.handle());
    CHECK(named.at("first").handle() == many[0].handle());

    put(many[0], "m", "1");
    CHECK(Store::total_count({many[0], many[1], fork}, named, info) == 6);
    CHECK(Store::total_count({many[0], many[1], fork}, named, std::nullopt) == 5);
    CHECK(s.count() == 1 && fork.count() == 2 && many[0].count() == 1);
}

// ── stats and report ────────────────────────────────────────────────────────

static void stats_and_report() {
    Store s = Store::open("/stats");
    put(s, "b", "12");
    put(s, "a", "1");
    put(s, "a", "123");
    put(s, "c", "x", EntryKind::Encrypted);

    Stats st = stats::summarize(s, std::nullopt);
    CHECK(st.entries == 3 && st.bytes == 6 && st.by_kind.size() == 2);
    CHECK(st.by_kind.at(EntryKind::Persistent) == 2 && st.by_kind.at(EntryKind::Encrypted) == 1);
    expect_key_not_found([&] { stats::summarize(s, std::string("q")); }, "q", "summarize('q')");

    std::vector<Entry> entries;
    for (Entry& e : s.entries(std::nullopt)) entries.push_back(std::move(e));
    std::vector<std::string> lines = report::render_report(entries);
    CHECK((lines == std::vector<std::string>{"a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent",
                                             "c: 1 bytes, Encrypted"}));
    NothingToReportError none =
        expect_throw<NothingToReportError>([] { report::render_report({}); }, "empty report");
    CHECK(none.code() == 2001 && std::string(none.what()) == "nothing to report");
    CHECK(dynamic_cast<const ReportError*>(&none) != nullptr);
    CHECK(dynamic_cast<const KvError*>(&none) == nullptr);
}

// ── scorer and the ABI 5 shapes ─────────────────────────────────────────────

enum class ScoreMode { Sizes, Fail, Short };

// Scores each value size as itself; fails, or returns too few scores, on
// request.
class TestScorer : public Scorer {
    ScoreMode mode_;

public:
    explicit TestScorer(ScoreMode mode) : mode_(mode) {}
    ~TestScorer() override { ++g_scorers_freed; }

    std::vector<double> scores(const std::vector<uint64_t>& sizes) override {
        if (mode_ == ScoreMode::Sizes) CHECK((sizes == std::vector<uint64_t>{1, 3, 2}));
        if (mode_ == ScoreMode::Fail) throw std::runtime_error("scorer is out of order");
        std::vector<double> out;
        for (uint64_t size : sizes) out.push_back(static_cast<double>(size));
        if (mode_ == ScoreMode::Short) out.resize(1);
        return out;
    }
};

static void abi5_shapes() {
    Store s = Store::open("/abi5");
    put(s, "b", "12");
    put(s, "a", "\x01\x02\x03", EntryKind::Volatile, 7);
    CHECK(s.count() == 2);

    // An optional scalar return.
    CHECK(s.expires_at("a") == 7);
    CHECK(!s.expires_at("b").has_value());
    CHECK(!s.expires_at("zzz").has_value());

    // A typed-array return, in key order.
    CHECK((s.value_sizes() == std::vector<uint64_t>{3, 2}));

    // Optional scalar iterator items: an absent item isn't the end.
    std::vector<std::optional<int64_t>> expirations;
    for (const std::optional<int64_t>& at : s.expirations()) expirations.push_back(at);
    CHECK((expirations == std::vector<std::optional<int64_t>>{7, std::nullopt}));

    // An optional scalar async result and a typed-array async result.
    CHECK(s.version_of("a").get() == 1u);
    CHECK(!s.version_of("q").get().has_value());
    put(s, "b", "x");
    CHECK((s.versions({"b", "q", "a"}).get() == std::vector<uint32_t>{2, 0, 1}));

    // A callback taking and returning typed arrays.
    Store r = Store::open("/rank");
    put(r, "a", "1");
    put(r, "b", "333");
    put(r, "c", "22");
    CHECK((r.rank(std::make_shared<TestScorer>(ScoreMode::Sizes)) == std::vector<std::string>{"b", "c", "a"}));
    CallbackFailedError failed = expect_throw<CallbackFailedError>(
        [&] { r.rank(std::make_shared<TestScorer>(ScoreMode::Fail)); }, "failing scorer");
    CHECK(failed.message == "scorer is out of order");
    failed = expect_throw<CallbackFailedError>(
        [&] { r.rank(std::make_shared<TestScorer>(ScoreMode::Short)); }, "short scorer");
    CHECK(failed.message == "expected 3 scores, got 1");
    CHECK(g_scorers_freed == 3);

    // `throws: any` and a `usize` return: the root Error with code -1.
    Store imp = Store::open("/import");
    CHECK(imp.import_lines("a=1\n\nb=two\n") == 2u);
    CHECK(imp.get("b").value == bytes("two"));
    Error e = expect_throw<Error>([&] { imp.import_lines("c=3\nbroken\nd=4"); }, "import_lines");
    CHECK(e.code() == -1 && std::string(e.what()) == "line 2: expected key=value");
    CHECK(dynamic_cast<const KvError*>(&e) == nullptr);
    CHECK(imp.count() == 3);
}

int main() {
    load_checks();
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

    CHECK(kvstore_debug_live(1) == 0);
    check_no_leaks(kvstore_debug_live, "kvstore");
    std::printf("cpp/kvstore: OK (%d listeners, %d policies, %d loaders, %d scorers released)\n",
                g_listeners_freed.load(), g_policies_freed.load(), g_loaders_freed.load(),
                g_scorers_freed.load());
    return 0;
}
