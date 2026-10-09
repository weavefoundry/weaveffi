// Conformance consumer: kvstore sample, Swift target (ABI revision 5).
//
// Drives the feature-complete producer through the generated `Kvstore`
// module:
//
//   * the load check (ABI revision, the `kv` and `report` contract tables),
//     through `KvstoreLibrary.check()`;
//   * the `Store` class: a throwing static factory and an `init`, methods,
//     statics, the deprecated `size()`, records (`Entry`, `StoreInfo`), the
//     C-style `EntryKind`, maps and optionals, and the logical clock;
//   * `KvError` cases with their payload fields (`keyNotFound`, `expired`,
//     `storeFull`, `rejected`, `invalidPath`);
//   * lazy sequences of strings (throwing), records, and objects;
//   * four callback protocols implemented in Swift: a `Listener` (retained,
//     filtered by `accepts`, told about every `Change`, detached when it
//     throws, and notified from a producer thread during compaction), a
//     `Policy` (an optional-scalar parameter and return in `ttlFor`, a record
//     return, a throwing method whose `KvError` and fields reach the `put`
//     caller with the domain's message, any other error, including a code
//     `KvError` doesn't declare, arriving as `callbackFailed`, an object
//     parameter and object return), a `Loader` passed as an optional
//     callback (string, bytes, and optional-object returns; `KvError`s
//     decoded by the producer or passed through), and a `Scorer` (typed
//     arrays in and out);
//   * `Store` objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * async calls: an async free function returning an object, a cancellable
//     method cancelled mid-pause by cancelling its task (throwing
//     `CancellationError` while its background work stops, shown by
//     `activeJobs()`), an async list, an async function in the nested
//     `kv.stats` module, and concurrent calls;
//   * the nested `kv.stats` module and the sibling `report` root;
//   * the ABI 5 shapes: an optional scalar return (`expiresAt`), iterator
//     item (`expirations`), and async result (`versionOf`); typed arrays as
//     a return (`valueSizes`) and an async result (`versions`); a `usize`
//     return and `throws: any` (`importLines`);
//   * object identity: wrappers of one native object are `==` and hash
//     alike, so records carrying objects are `Hashable` too.
//
// Ends by asserting the producer's leak counters are zero and that the
// library released every callback implementation. Exits non-zero on any
// mismatch.

import CKvstore
import Foundation
import Kvstore

func fail(_ msg: String, line: UInt = #line) -> Never {
    FileHandle.standardError.write(Data("assertion failed (line \(line)): \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: @autoclosure () -> String, line: UInt = #line) {
    if !cond { fail(msg(), line: line) }
}

/// Every live-resource counter must settle at zero: 0 objects, 1 callbacks,
/// 2 iterators, 3 cancel tokens, 4 allocations. A producer worker may still
/// be dropping a finished async call, so the counters get two seconds.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<2000 {
        live = (0..<5).map { kvstore_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(1000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

/// Run `body`, which must throw the `KvError` `check` accepts.
func expectKvError<T>(_ what: String, line: UInt = #line, _ body: () throws -> T, _ check: (KvError) -> Bool) {
    do {
        _ = try body()
        fail("\(what) returned", line: line)
    } catch let error as KvError {
        expect(check(error), "\(what) threw \(error)", line: line)
    } catch {
        fail("\(what) threw \(error), not KvError", line: line)
    }
}

/// Run `body`, which must throw the runtime error with `code` and `message`.
func expectRuntimeError<T>(_ what: String, code: Int32, message: String, line: UInt = #line, _ body: () throws -> T) {
    do {
        _ = try body()
        fail("\(what) returned", line: line)
    } catch let error as KvstoreRuntimeError {
        expect(error.errorCode == code && error.message == message, "\(what) threw \(error)", line: line)
    } catch {
        fail("\(what) threw \(error), not KvstoreRuntimeError", line: line)
    }
}

func isKeyNotFound(_ key: String) -> (KvError) -> Bool {
    { error in
        guard case let .keyNotFound(_, got) = error else { return false }
        return got == key && error.errorCode == 1001
    }
}

/// `true` when `a` and `b` wrap the same native store: a write through one
/// is visible through the other.
func isSame(_ a: Store, _ b: Store) -> Bool {
    let probe = "identity-probe"
    _ = try! a.put(key: probe, value: Data(), kind: .volatile, ttlSeconds: nil)
    defer { _ = a.delete(key: probe) }
    return b.find(key: probe) != nil
}

@discardableResult
func put(_ s: Store, _ key: String, _ value: String, _ kind: EntryKind = .persistent, ttl: Int64? = nil) -> Entry {
    do {
        return try s.put(key: key, value: Data(value.utf8), kind: kind, ttlSeconds: ttl)
    } catch {
        fail("put(\(key)) threw \(error)")
    }
}

func open(_ path: String) -> Store {
    do {
        return try Store.open(path: path)
    } catch {
        fail("open(\(path)) threw \(error)")
    }
}

/// A thread-safe tally shared by the test and the callbacks it creates.
final class Tally: @unchecked Sendable {
    /// The thread that created the tally: synchronous calls must notify on
    /// it. (`Thread.isMainThread` isn't reliable for this on Linux, where
    /// top-level async code doesn't run on the main thread.)
    let origin = pthread_self()
    private let lock = NSLock()
    private var counts: [String: Int] = [:]
    private var notes: [String: String] = [:]

    func add(_ name: String) {
        lock.lock()
        counts[name, default: 0] += 1
        lock.unlock()
    }

    subscript(name: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return counts[name, default: 0]
    }

    func note(_ name: String, _ value: String) {
        lock.lock()
        notes[name] = value
        lock.unlock()
    }

    func noted(_ name: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return notes[name]
    }
}

/// Every callback implementation the library released, by kind.
enum Released {
    static let tally = Tally()
}

struct Refused: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}

// MARK: Callback implementations

/// Records every change into `tally`; refuses to look at `failOn`.
final class RecordingListener: Listener, @unchecked Sendable {
    let tally: Tally
    let skip: String?
    let failOn: String?

    init(_ tally: Tally, skip: String? = nil, failOn: String? = nil) {
        self.tally = tally
        self.skip = skip
        self.failOn = failOn
    }

    deinit {
        Released.tally.add("listener")
    }

    func accepts(key: String) throws -> Bool {
        if key == failOn { throw Refused("listener refused") }
        return key != skip
    }

    func onChange(change: Change) {
        if pthread_equal(pthread_self(), tally.origin) == 0 { tally.add("offMain") }
        switch change {
        case let .put(entry, replaced):
            tally.note("version", String(entry.version))
            tally.note("replaced", String(replaced))
            tally.note("key", entry.key)
            tally.add("puts")
        case let .removed(key, expired):
            tally.note("key", key)
            tally.add(expired ? "expired" : "removed")
        case let .cleared(count):
            tally.note("cleared", String(count))
            tally.add("cleared")
        }
    }
}

/// Tags admitted entries, vetoes secrets with a typed error, fails on
/// "boom", and routes "b/" keys to `other`.
final class TestPolicy: Policy, @unchecked Sendable {
    let tally: Tally
    let other: Store

    init(_ tally: Tally, other: Store) {
        self.tally = tally
        self.other = other
    }

    deinit {
        Released.tally.add("policy")
    }

    /// "short" lives one tick, "forever" never expires, "ttl-fail" fails
    /// with a typed error, and every other key keeps the requested TTL.
    func ttlFor(key: String, requested: Int64?) throws -> Int64? {
        switch key {
        case "short": return 1
        case "forever": return nil
        case "ttl-fail": throw KvError.invalidPath(message: "no ttl for you")
        default: return requested
        }
    }

    func admit(entry: Entry) throws -> Entry {
        tally.add("admitted")
        expect(entry.version == 0, "admit sees version 0")
        if entry.key.hasPrefix("secret") {
            throw KvError.rejected(message: "secrets are not stored", key: entry.key, reason: "no secrets")
        }
        if entry.key.hasPrefix("boom") {
            throw Refused("policy exploded")
        }
        if entry.key.hasPrefix("unknown") {
            // A code the domain doesn't declare is a plain callback failure.
            throw KvError.unknown(code: 4242, message: "no such code")
        }
        var admitted = entry
        admitted.tags = ["admitted"]
        admitted.kind = .encrypted
        admitted.key = "renamed"  // the store keeps the original key
        return admitted
    }

    func route(key: String, home: Store) throws -> Store {
        key.hasPrefix("b/") ? other : home
    }
}

/// Loads "loaded:{key}", falls back to `backup` for "fb", and fails for
/// "missing" (this key), "elsewhere" (another key), and "broken".
final class TestLoader: Loader, @unchecked Sendable {
    let backup: Store?

    init(backup: Store? = nil) {
        self.backup = backup
    }

    deinit {
        Released.tally.add("loader")
    }

    func name() throws -> String {
        "swift-loader"
    }

    func fallback(key: String) throws -> Store? {
        key == "fb" ? backup : nil
    }

    func load(key: String) throws -> Data {
        switch key {
        case "missing": throw KvError.keyNotFound(message: "not in the loader", key: "missing")
        case "elsewhere": throw KvError.keyNotFound(message: "not in the loader", key: "other")
        case "broken": throw Refused("loader is broken")
        default: return Data("loaded:\(key)".utf8)
        }
    }
}

/// Scores each value size as itself, or fails, or returns too few scores.
final class TestScorer: Scorer, @unchecked Sendable {
    enum Mode { case sizes, fail, short }
    let mode: Mode

    init(_ mode: Mode) {
        self.mode = mode
    }

    deinit {
        Released.tally.add("scorer")
    }

    func scores(sizes: [UInt64]) throws -> [Double] {
        switch mode {
        case .sizes:
            expect(sizes == [1, 3, 2], "scores receives the sizes in key order (got \(sizes))")
            return sizes.map(Double.init)
        case .fail:
            throw Refused("scorer is out of order")
        case .short:
            return [Double(sizes[0])]
        }
    }
}

/// `true` when `error` is `KvError.callbackFailed` with `message`.
func isCallbackFailed(_ message: String) -> (KvError) -> Bool {
    { error in
        guard case let .callbackFailed(rendered, field) = error else { return false }
        return field == message && rendered == message && error.errorCode == 1006
    }
}

/// Calls the deprecated `size()` through a protocol witness, which doesn't
/// warn at the call site.
protocol Sized {
    func size() -> UInt32
}

extension Store: Sized {}

// MARK: Sections

func constructors() async {
    expectKvError("open(\"\")", { try Store.open(path: "") }) { error in
        guard case let .invalidPath(message) = error else { return false }
        return message == "invalid path" && error.errorCode == 1004
    }
    let s = Store()
    expect(s.path() == "memory", "Store() path")
    expect(s.capacity() == Store.defaultCapacity() && Store.defaultCapacity() == 1_000_000, "capacity")

    do {
        let opened = try await Kv.openStore(path: "/async")
        expect(opened.path() == "/async", "openStore path")
    } catch {
        fail("openStore threw \(error)")
    }
    do {
        _ = try await Kv.openStore(path: "")
        fail("openStore(\"\") returned")
    } catch let error as KvError {
        guard case let .invalidPath(message) = error else { fail("openStore(\"\") threw \(error)") }
        expect(message == "invalid path", "openStore(\"\") message")
    } catch {
        fail("openStore(\"\") threw \(error)")
    }
}

func basics() {
    let s = open("/basics")
    let first = put(s, "alpha", "one")
    expect(first == Entry(key: "alpha", value: Data("one".utf8), kind: .persistent, version: 1, expiresAt: nil, tags: [], metadata: [:]), "first put \(first)")
    let second = put(s, "alpha", "two", .volatile)
    expect(second.version == 2 && second.kind == .volatile, "second put")
    expect((try? s.get(key: "alpha"))?.value == Data("two".utf8), "get alpha")
    expectKvError("get(nope)", { try s.get(key: "nope") }) { error in
        isKeyNotFound("nope")(error) && error.localizedDescription == "key not found: nope"
    }
    expect(s.find(key: "alpha")?.version == 2 && s.find(key: "nope") == nil, "find")

    // TTLs follow the logical clock; an expired get reports when.
    expect(put(s, "ttl", "x", .volatile, ttl: 10).expiresAt == 10, "expiresAt")
    expect(s.now() == 0, "now starts at 0")
    expect(s.tick(seconds: 9) == 9 && s.count() == 2, "tick 9")
    expect(s.tick(seconds: 1) == 10 && s.count() == 1, "tick 10")
    expectKvError("get(ttl)", { try s.get(key: "ttl") }) { error in
        guard case let .expired(_, key, expiredAt) = error else { return false }
        return key == "ttl" && expiredAt == 10
    }
    expectKvError("get(ttl) again", { try s.get(key: "ttl") }, isKeyNotFound("ttl"))

    // Capacity: a new key past it is StoreFull { capacity }.
    s.setCapacity(capacity: 1)
    expect(s.capacity() == 1, "setCapacity")
    put(s, "alpha", "three")
    expectKvError("put(beta)", { try s.put(key: "beta", value: Data(), kind: .volatile, ttlSeconds: nil) }) { error in
        guard case let .storeFull(_, capacity) = error else { return false }
        return capacity == 1
    }
    s.setCapacity(capacity: 100)

    put(s, "beta", "b")
    expect(s.delete(key: "beta") && !s.delete(key: "beta"), "delete")
    expect(UInt64((s as Sized).size()) == s.count() && s.count() == 1, "deprecated size")
    expect(s.clear() == 1 && s.count() == 0, "clear")
}

func iterators() {
    let s = open("/iter")
    put(s, "user.bob", "b")
    put(s, "user.alice", "a")
    put(s, "sys.x", "xx")

    do {
        let keys = try s.keys(prefix: nil)
        expect(Array(keys) == ["sys.x", "user.alice", "user.bob"], "keys in order")
        expect(keys.error == nil && keys.next() == nil, "keys stays at the end")
    } catch {
        fail("keys threw \(error)")
    }
    expectKvError("keys(zzz)", { try s.keys(prefix: "zzz") }, isKeyNotFound("zzz"))

    // Abandoning a sequence part-way releases the native iterator.
    do {
        let keys = try s.keys(prefix: "user.")
        withExtendedLifetime(keys) {
            expect(keys.next() == "user.alice", "first user key")
            expect(kvstore_debug_live(2) == 1, "one live iterator")
        }
    } catch {
        fail("keys(user.) threw \(error)")
    }
    expect(kvstore_debug_live(2) == 0, "the abandoned iterator was destroyed")

    let entries = Array(s.entries(prefix: "sys."))
    expect(entries.count == 1 && entries[0].key == "sys.x" && entries[0].value == Data("xx".utf8), "entries(sys.)")

    let prefixes = ["user.", "sys.", "none."]
    let parts = Array(s.partition(prefixes: prefixes))
    expect(parts.map { $0.count() } == [2, 1, 0], "partition counts")
    expect(parts.map { $0.path() } == prefixes, "partition paths")
    expect(!parts.contains { isSame($0, s) }, "partitions are new stores")
}

func listeners() {
    let tally = Tally()
    let store = open("/listen")

    let id = store.subscribe(listener: RecordingListener(tally, skip: "quiet"))
    expect(id > 0 && store.listenerCount() == 1, "subscribe")

    put(store, "a", "1")
    expect(tally["puts"] == 1 && tally.noted("version") == "1" && tally.noted("replaced") == "false", "first Put")
    put(store, "a", "2")
    expect(tally["puts"] == 2 && tally.noted("version") == "2" && tally.noted("replaced") == "true", "second Put")
    expect(tally.noted("key") == "a", "Put key")
    put(store, "quiet", "x")
    expect(tally["puts"] == 2, "accepts filtered quiet")
    expect(store.delete(key: "a") && tally["removed"] == 1 && tally.noted("key") == "a", "Removed")

    // An expired read removes the entry and says so.
    put(store, "short", "x", .volatile, ttl: 1)
    _ = store.tick(seconds: 1)
    expectKvError("get(short)", { try store.get(key: "short") }) { error in
        if case .expired = error { return true }
        return false
    }
    expect(tally["expired"] == 1 && tally.noted("key") == "short", "Removed(expired)")
    expect(store.clear() == 1 && tally["cleared"] == 1 && tally.noted("cleared") == "1", "Cleared")
    expect(tally["offMain"] == 0, "synchronous calls notify on the calling thread")

    // Unsubscribing releases the listener once.
    expect(store.unsubscribe(id: id) && Released.tally["listener"] == 1, "unsubscribe releases")
    expect(!store.unsubscribe(id: id) && store.listenerCount() == 0, "unsubscribe again")

    // A listener that throws is detached (and released); the put succeeds.
    let failing = Tally()
    _ = store.subscribe(listener: RecordingListener(failing, failOn: "boom"))
    put(store, "fine", "1")
    expect(failing["puts"] == 1, "failing listener saw fine")
    put(store, "boom", "1")
    expect(store.count() == 2 && store.listenerCount() == 0, "failing listener detached")
    expect(Released.tally["listener"] == 2, "failing listener released")

    // Releasing the store releases the listeners it still holds.
    _ = store.subscribe(listener: RecordingListener(Tally()))
    _ = store.subscribe(listener: RecordingListener(Tally()))
    expect(store.listenerCount() == 2, "two listeners")
}

func listenersReleasedWithStore() {
    listeners()
    expect(Released.tally["listener"] == 4, "releasing the store released its listeners (got \(Released.tally["listener"]))")
}

func policies() {
    let tally = Tally()
    let s = open("/policy")
    let other = open("/other")

    s.setPolicy(policy: TestPolicy(tally, other: other))
    expect(s.hasPolicy(), "hasPolicy")

    // admit's record return is what's stored (its key and version aside).
    let a = put(s, "a", "1", .volatile)
    expect(a.key == "a" && a.version == 1 && a.kind == .encrypted && a.tags == ["admitted"], "admitted entry \(a)")

    // route: the object parameter and object return redirect a write.
    put(s, "b/x", "2", .volatile)
    expect(s.count() == 1 && other.count() == 1, "route redirected b/x")

    // A typed error from the throwing callback reaches the caller with its
    // code and fields, and the domain's own message for them.
    expectKvError("put(secret)", { try s.put(key: "secret", value: Data(), kind: .volatile, ttlSeconds: nil) }) { error in
        error == .rejected(message: "write to secret rejected: no secrets", key: "secret", reason: "no secrets") && error.errorCode == 1005
    }
    // Any other error, and a code the domain doesn't declare, arrive as
    // `callbackFailed` with the consumer's message.
    expectKvError("put(boom)", { try s.put(key: "boom", value: Data(), kind: .volatile, ttlSeconds: nil) }, isCallbackFailed("policy exploded"))
    expectKvError("put(unknown)", { try s.put(key: "unknown", value: Data(), kind: .volatile, ttlSeconds: nil) }, isCallbackFailed("no such code"))
    expect(s.count() == 1 && other.count() == 1, "failed puts stored nothing")
    expect(tally["admitted"] == 5, "admit calls")

    // ttlFor: an optional scalar in and out, consulted before admit.
    expect(put(s, "short", "x", .volatile).expiresAt == 1, "ttlFor(short)")
    expect(put(s, "forever", "x", .volatile, ttl: 5).expiresAt == nil, "ttlFor(forever)")
    expect(put(s, "kept", "x", .volatile, ttl: 5).expiresAt == 5, "ttlFor keeps the requested TTL")
    expectKvError("put(ttl-fail)", { try s.put(key: "ttl-fail", value: Data(), kind: .volatile, ttlSeconds: nil) }) { error in
        error == .invalidPath(message: "invalid path")
    }
    expect(s.count() == 4 && tally["admitted"] == 8, "ttlFor puts")

    // Replacing the policy releases the old one; nil removes it.
    s.setPolicy(policy: TestPolicy(tally, other: other))
    expect(Released.tally["policy"] == 1, "replaced policy released")
    s.setPolicy(policy: nil)
    expect(Released.tally["policy"] == 2 && !s.hasPolicy(), "removed policy released")
    put(s, "secret", "now allowed")
    expect(s.count() == 5, "no policy, no veto")
}

func loaders() {
    let s = open("/load")
    func load(_ key: String, _ loader: TestLoader?) throws -> Entry? {
        try s.getOrLoad(key: key, loader: loader)
    }
    func loadOK(_ key: String, _ loader: TestLoader?) -> Entry? {
        do {
            return try load(key, loader)
        } catch {
            fail("getOrLoad(\(key)) threw \(error)")
        }
    }

    // No loader (a nil optional callback): a miss is nil.
    expect(loadOK("k", nil) == nil, "no loader")

    // load's bytes are stored, tagged with the loader's name.
    guard let loaded = loadOK("k", TestLoader()) else { fail("getOrLoad(k)") }
    expect(loaded.value == Data("loaded:k".utf8) && loaded.kind == .volatile, "loaded entry")
    expect(loaded.metadata == ["source": "swift-loader"], "loaded metadata")
    expect(Released.tally["loader"] == 1, "a loader is released after the call")
    expect(s.count() == 1, "loaded entry stored")
    // A hit doesn't consult the loader.
    expect(loadOK("k", TestLoader())?.version == 1, "hit")

    // The fallback store (an optional object return) is consulted first.
    let backup = open("/backup")
    put(backup, "fb", "from backup")
    guard let copied = loadOK("fb", TestLoader(backup: backup)) else { fail("getOrLoad(fb)") }
    expect(copied.value == Data("from backup".utf8) && copied.kind == .persistent, "fallback entry")

    // KeyNotFound for this key: the producer decoded the payload and
    // answers nil.
    expect(loadOK("missing", TestLoader()) == nil, "missing")
    // KeyNotFound for another key: passed through, payload intact, with the
    // domain's message.
    expectKvError("getOrLoad(elsewhere)", { try load("elsewhere", TestLoader()) }) { error in
        error.localizedDescription == "key not found: other" && isKeyNotFound("other")(error)
    }
    // Any other failure is `callbackFailed` with the loader's message.
    expectKvError("getOrLoad(broken)", { try load("broken", TestLoader()) }, isCallbackFailed("loader is broken"))
    expect(Released.tally["loader"] == 6, "every loader released (got \(Released.tally["loader"]))")
}

@MainActor
func asyncCalls() async {
    let tally = Tally()
    let s = open("/async-calls")
    _ = s.subscribe(listener: RecordingListener(tally))
    put(s, "old1", "x", .volatile, ttl: 1)
    put(s, "old2", "x", .volatile, ttl: 1)
    put(s, "keep", "x")
    _ = s.tick(seconds: 5)

    do {
        // compact runs on a producer thread and notifies listeners there.
        expect(try await s.compact(pauseMs: 0) == 2, "compact(0)")
        expect(tally["expired"] == 2 && tally["offMain"] == 2, "compaction notified from a producer thread")
        expect(s.count() == 1, "compacted")
        expect(try await s.compact(pauseMs: 5) == 0, "compact(5)")
    } catch {
        fail("compact threw \(error)")
    }

    // Cancelling the task cancels the call mid-pause: it throws
    // CancellationError at once, and the background pause stops.
    let pause = Task { try await s.compact(pauseMs: 60_000) }
    try? await Task.sleep(nanoseconds: 20_000_000)
    expect(Store.activeJobs() >= 1, "the pause is running")
    let cancelledAt = Date()
    pause.cancel()
    do {
        _ = try await pause.value
        fail("the cancelled compact returned")
    } catch is CancellationError {
        expect(Date().timeIntervalSince(cancelledAt) < 2, "cancelled promptly")
    } catch {
        fail("the cancelled compact threw \(error)")
    }
    var stopped = false
    for _ in 0..<2000 where !stopped {
        stopped = Store.activeJobs() == 0
        if !stopped { usleep(1000) }
    }
    expect(stopped, "the cancelled pause stopped cooperatively")

    // getMany: an async list of optional records, called concurrently.
    let results = await withTaskGroup(of: [Entry?].self) { group in
        for _ in 0..<32 {
            group.addTask { await s.getMany(keys: ["keep", "gone", "keep"]) }
        }
        var all: [[Entry?]] = []
        for await r in group { all.append(r) }
        return all
    }
    expect(results.count == 32, "32 getMany calls")
    for r in results {
        expect(r.count == 3 && r[0]?.key == "keep" && r[1] == nil && r[2]?.key == "keep", "getMany result \(r)")
    }

    // The nested module's async function: objects in a list in, a record out.
    let other = open("/other")
    put(other, "a", "123")
    let stats = await Kv.Stats.summarizeAll(stores: [s, other])
    expect(stats == Stats(entries: 2, bytes: 4, byKind: [.persistent: 2]), "summarizeAll \(stats)")
}

func objectGraph() {
    var original: Store? = open("/graph")
    put(original!, "k", "v")

    // share(): the same object (so `==`); the original reference can go.
    let s = original!.share()
    expect(isSame(s, original!) && s == original! && s.hashValue == original!.hashValue, "share is the same store")
    original = nil
    _ = original
    expect(s.count() == 1, "alive through the shared reference")

    // fork(): a distinct object with a copy of the entries.
    let fork = s.fork()
    expect(!isSame(fork, s) && fork != s && fork.count() == 1 && fork.path() == "/graph", "fork")
    put(fork, "k2", "v")
    expect(fork.count() == 2 && s.count() == 1, "fork is independent")

    // larger(): `Store?` in and out.
    let empty = open("/empty")
    expect(empty.larger(other: nil) == nil, "larger(nil) on an empty store")
    expect(empty.larger(other: fork) == fork, "larger(fork)")
    expect(s.larger(other: nil) == s, "larger(nil) is self")
    expect(Set([s, s.share(), fork, s.larger(other: nil)!]).count == 2, "stores hash by identity")

    // describe(): a record whose fields carry objects.
    let info = s.describe(label: "main", mirror: fork)
    expect(info.label == "main" && info.store == s && info.count == 1, "describe")
    expect(info.mirror == fork && info.mirror?.count() == 2, "describe mirror")
    // A record carrying objects compares by their identity.
    expect(info == StoreInfo(label: "main", store: s.share(), mirror: fork, count: 1), "StoreInfo ==")
    expect(info != StoreInfo(label: "main", store: s.fork(), mirror: fork, count: 1), "StoreInfo != for another store")

    // openMany(): a list of objects; one bad path fails the whole call.
    guard let many = try? Store.openMany(paths: ["/a", "/b"]) else { fail("openMany") }
    expect(many.map { $0.path() } == ["/a", "/b"], "openMany paths")
    expectKvError("openMany([/a, \"\"])", { try Store.openMany(paths: ["/a", ""]) }) { error in
        if case .invalidPath = error { return true }
        return false
    }

    // byLabel(): records with objects in, a map with object values out.
    let named = Store.byLabel(infos: [info, StoreInfo(label: "first", store: many[0], mirror: nil, count: 0)])
    expect(named.count == 2, "byLabel count")
    expect(named["main"] == s, "byLabel main")
    expect(named["first"] == many[0], "byLabel first")

    // totalCount(): a list, a map, and an optional record, all carrying
    // objects (each encoded as a fresh reference the producer adopts).
    put(many[0], "m", "1")
    // stores: 1 + 0 + 2; named: main 1 + first 1; extra: main 1.
    expect(Store.totalCount(stores: [many[0], many[1], fork], named: named, extra: info) == 6, "totalCount")
    expect(Store.totalCount(stores: [many[0], many[1], fork], named: named, extra: nil) == 5, "totalCount without extra")

    // Everything is still intact after crossing in buffers.
    expect(s.count() == 1 && fork.count() == 2 && many[0].count() == 1, "objects intact")
}

func statsAndReport() {
    let s = open("/stats")
    put(s, "b", "12")
    put(s, "a", "1")
    put(s, "a", "123")
    put(s, "c", "x", .encrypted)

    // kv.stats.summarize: the parent's Store as a parameter, the parent's
    // error domain for a prefix that matches nothing.
    do {
        let stats = try Kv.Stats.summarize(store: s, prefix: nil)
        expect(stats == Stats(entries: 3, bytes: 6, byKind: [.persistent: 2, .encrypted: 1]), "summarize \(stats)")
    } catch {
        fail("summarize threw \(error)")
    }
    expectKvError("summarize(q)", { try Kv.Stats.summarize(store: s, prefix: "q") }, isKeyNotFound("q"))

    // report.render_report: the sibling root shares the Entry record.
    do {
        let lines = try Report.renderReport(entries: Array(s.entries(prefix: nil)))
        expect(lines == ["a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent", "c: 1 bytes, Encrypted"], "renderReport \(lines)")
    } catch {
        fail("renderReport threw \(error)")
    }
    do {
        _ = try Report.renderReport(entries: [])
        fail("renderReport([]) returned")
    } catch let error as ReportError {
        guard case let .nothingToReport(message) = error else { fail("renderReport([]) threw \(error)") }
        expect(message == "nothing to report" && error.errorCode == 2001, "NothingToReport")
    } catch {
        fail("renderReport([]) threw \(error)")
    }
}

@MainActor
func abi5Shapes() async {
    let s = open("/abi5")
    put(s, "b", "12")
    _ = try! s.put(key: "a", value: Data([1, 2, 3]), kind: .volatile, ttlSeconds: 7)
    expect(s.count() == 2, "count")

    // An optional scalar return.
    expect(s.expiresAt(key: "a") == 7, "expiresAt(a)")
    expect(s.expiresAt(key: "b") == nil && s.expiresAt(key: "zzz") == nil, "expiresAt absent")

    // A typed-array return, in key order.
    expect(s.valueSizes() == [3, 2], "valueSizes")

    // Optional scalar iterator items.
    let expirations = s.expirations()
    expect(Array(expirations) == [7, nil], "expirations")
    expect(expirations.error == nil && expirations.next() == nil, "expirations ended")

    // An optional scalar and a typed-array async result.
    let a = await s.versionOf(key: "a")
    let q = await s.versionOf(key: "q")
    expect(a == 1 && q == nil, "versionOf (got \(String(describing: a)), \(String(describing: q)))")
    put(s, "b", "x")
    let versions = await s.versions(keys: ["b", "q", "a"])
    expect(versions == [2, 0, 1], "versions (got \(versions))")

    // A callback taking and returning typed arrays.
    let r = open("/rank")
    put(r, "a", "1")
    put(r, "b", "333")
    put(r, "c", "22")
    do {
        expect(try r.rank(scorer: TestScorer(.sizes)) == ["b", "c", "a"], "rank")
    } catch {
        fail("rank threw \(error)")
    }
    expectKvError("rank(fail)", { try r.rank(scorer: TestScorer(.fail)) }, isCallbackFailed("scorer is out of order"))
    expectKvError("rank(short)", { try r.rank(scorer: TestScorer(.short)) }, isCallbackFailed("expected 3 scores, got 1"))
    expect(Released.tally["scorer"] == 3, "every scorer released")

    // `throws: any` and a `usize` return: the runtime error, code -1.
    let i = open("/import")
    do {
        expect(try i.importLines(text: "a=1\n\nb=two\n") == 2, "importLines")
        expect(try i.get(key: "b").value == Data("two".utf8), "imported b")
    } catch {
        fail("importLines threw \(error)")
    }
    expectRuntimeError("importLines(broken)", code: KvstoreRuntimeError.untypedCode, message: "line 2: expected key=value") {
        try i.importLines(text: "c=3\nbroken\nd=4")
    }
    expect(i.count() == 3, "c was stored, d wasn't")
}

do {
    try KvstoreLibrary.check()
} catch {
    fail("check() threw \(error)")
}
await constructors()
basics()
iterators()
listenersReleasedWithStore()
policies()
loaders()
await asyncCalls()
objectGraph()
statsAndReport()
await abi5Shapes()

expect(kvstore_abi_version() == 5, "ABI revision 5")
expect(kvstore_debug_live(-1) == 1, "the sample counts live resources")
assertNoLeaks()
expect(Released.tally["listener"] == 5, "every listener released (got \(Released.tally["listener"]))")
expect(Released.tally["policy"] == 2, "every policy released (got \(Released.tally["policy"]))")
print("swift/kvstore: OK")
