// Conformance consumer: kvstore sample, Swift target.
//
// Binds through the generated `Kvstore` module and exercises the ABI 2
// surface: `Store` as a reference-counted final class opened via the throwing
// static factory `Store.open(path:)`, throwing methods raising the typed
// `KvError` domain enum (put/get/delete/listKeys), non-throwing methods
// without `try` (count/clear), the buffered optional parameter (`ttlSeconds`),
// `Entry` decoded from a value buffer, the `listKeys` iterator as a lazy
// Sequence, the nested `Kv.Stats` submodule, the CheckedContinuation-backed
// `compact()` async method, the `EvictionListener` callback interface
// implemented as a Swift class (arguments observed, detach by returning
// false, replace/clear releasing the previous listener, a thrown error
// surfacing to the caller as `WeaveFFIError` with code -4), and the object
// graph: `share()` returning a wrapper to the same object, `fork()`,
// `larger(other:)` with `Store?` both ways, `describe(label:mirror:)`
// returning a record that carries objects, `openMany` returning a list of
// objects, and `totalCount` taking objects inside a list and a record.
// Release is observed through weak references to the wrappers, whose deinit
// calls the generated destroy. Typed-error asserts pin the case and the
// numeric code (keyNotFound 1001, expired 1002, ioError 1004).

import Foundation
import Kvstore

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

struct ListenerFailure: Error, LocalizedError {
    let key: String
    var errorDescription: String? { "listener rejected \(key)" }
}

/// A consumer-side eviction listener. Records every (key, reason) pair,
/// detaches itself by returning false once `keepAfter` evictions were seen,
/// and throws for `failKey` so the foreign-error path can be observed.
final class Listener: EvictionListener {
    var evicted: [(key: String, reason: EvictionReason)] = []
    var lastEntry: Entry?
    let keepAfter: Int
    let failKey: String?

    init(keepAfter: Int = Int.max, failKey: String? = nil) {
        self.keepAfter = keepAfter
        self.failKey = failKey
    }

    func onEvict(entry: Entry, reason: EvictionReason) throws -> Bool {
        if entry.key == failKey { throw ListenerFailure(key: entry.key) }
        evicted.append((entry.key, reason))
        lastEntry = entry
        return evicted.count < keepAfter
    }
}

do {
    let store = try Store.open(path: "/tmp/conformance-kvstore-swift")

    let payload = Data([1, 2, 3])
    _ = try store.put(key: "alpha", value: payload, kind: .persistent, ttlSeconds: nil)
    _ = try store.put(key: "beta", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(store.count() == 2, "count == 2")

    // A present key decodes the buffered `Entry?` into a struct value. The
    // producer stores no tags or metadata, so the nested list and map fields
    // decode as empty collections, and the absent TTL as nil.
    let fetched = try store.get(key: "alpha")
    expect(fetched?.key == "alpha", "get alpha key")
    expect(fetched?.value == payload, "get alpha value")
    expect(fetched?.expiresAt == nil, "get alpha expiresAt nil")
    expect(fetched?.tags == [], "get alpha tags empty")
    expect(fetched?.metadata == [:], "get alpha metadata empty")
    expect(fetched?.id == 1, "first entry id is 1 (got \(fetched?.id ?? -1))")

    // A TTL round-trips through the buffered optional in both directions.
    _ = try store.put(key: "timed", value: payload, kind: .volatile, ttlSeconds: 3600)
    let timed = try store.get(key: "timed")
    expect(timed?.expiresAt != nil && timed!.expiresAt! - timed!.createdAt == 3600,
           "ttl produces expiresAt = createdAt + 3600")
    expect(try store.delete(key: "timed"), "delete timed")

    // Iterator lowering: a lazy single-pass Sequence pulled one element per
    // step, drained here into [String] (the BTreeMap's sorted order). A
    // per-next producer error would end iteration and set `.error`.
    let keysIter = try store.listKeys(prefix: nil)
    let keys = Array(keysIter)
    expect(keysIter.error == nil, "listKeys iteration error-free")
    expect(keys == ["alpha", "beta"], "listKeys sorted (got \(keys))")
    expect(Array(try store.listKeys(prefix: "al")) == ["alpha"], "listKeys prefix filter")
    var seen: [String] = []
    for k in try store.listKeys(prefix: nil) { seen.append(k) }
    expect(seen == ["alpha", "beta"], "for-in over listKeys")
    expect(try store.listKeys(prefix: "zzz").next() == nil, "empty prefix match yields nothing")

    // A missing key raises the typed domain error's keyNotFound case (1001).
    do {
        _ = try store.get(key: "missing")
        fail("expected KvError.keyNotFound for missing key")
    } catch let e as KvError {
        guard case let .keyNotFound(message) = e else { fail("expected .keyNotFound, got \(e)") }
        expect(e.errorCode == 1001, "keyNotFound code == 1001 (got \(e.errorCode))")
        expect(message == "key not found", "keyNotFound message (got \(message))")
    }

    // The memberwise init builds a local Entry value with its list and map
    // fields populated; the struct's stored properties read straight back.
    let entry = Entry(
        id: 7, key: "alpha", value: payload, createdAt: 1000, expiresAt: nil,
        tags: ["hot", "fast"], metadata: ["source": "test", "env": "prod"])
    expect(entry.id == 7, "entry id")
    expect(entry.tags == ["hot", "fast"], "tags")
    expect(entry.metadata.count == 2 && entry.metadata["source"] == "test" && entry.metadata["env"] == "prod",
           "metadata")

    // Nested Kv.Stats submodule (name collides with the module-level Stats
    // struct), passing the Store interface across the module boundary and
    // decoding the Stats record from a value buffer.
    let stats = try Kv.Stats.getStats(store: store)
    expect(stats.totalEntries == 2, "stats totalEntries")
    expect(stats.totalBytes == 6, "stats totalBytes")
    expect(stats.expiredEntries == 0, "stats expiredEntries")

    // --- EvictionListener callback interface ---------------------------------
    var listener: Listener? = Listener()
    weak var weakListener = listener
    store.setEvictionListener(listener: listener!)
    expect(try store.delete(key: "beta"), "delete beta")
    expect(listener!.evicted.count == 1, "one eviction observed")
    expect(listener!.evicted[0].key == "beta" && listener!.evicted[0].reason == .deleted,
           "delete evicts beta with reason .deleted (got \(listener!.evicted))")
    expect(listener!.lastEntry?.value == payload && listener!.lastEntry?.id == 2,
           "the evicted Entry carried its payload and id")
    expect(!(try store.delete(key: "beta")), "second delete reports absent")
    expect(listener!.evicted.count == 1, "absent delete does not notify")

    // TTL expiry: reading an already-expired entry raises the expired case
    // (1002) and evicts it with reason .expired.
    _ = try store.put(key: "ttl", value: payload, kind: .volatile, ttlSeconds: -1)
    do {
        _ = try store.get(key: "ttl")
        fail("expected KvError.expired for expired entry")
    } catch let e as KvError {
        guard case .expired = e else { fail("expected .expired, got \(e)") }
        expect(e.errorCode == 1002, "expired code == 1002 (got \(e.errorCode))")
    }
    expect(listener!.evicted.count == 2 && listener!.evicted[1].key == "ttl"
           && listener!.evicted[1].reason == .expired,
           "expiry evicts ttl with reason .expired (got \(listener!.evicted))")
    expect(store.count() == 1, "expired entry left the store")

    // The store retains the listener after the consumer drops it, and
    // releases it (running the generated `free`) when it is replaced.
    listener = nil
    expect(weakListener != nil, "store retains the listener")
    var second: Listener? = Listener(keepAfter: 2)
    weak var weakSecond = second
    store.setEvictionListener(listener: second!)
    expect(weakListener == nil, "replaced listener is freed")

    // Returning false detaches (and frees) the listener; later evictions are
    // not observed.
    _ = try store.put(key: "one", value: payload, kind: .volatile, ttlSeconds: nil)
    _ = try store.put(key: "two", value: payload, kind: .volatile, ttlSeconds: nil)
    _ = try store.put(key: "three", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(try store.delete(key: "one"), "delete one")
    expect(try store.delete(key: "two"), "delete two")
    expect(second!.evicted.map { $0.key } == ["one", "two"], "second saw two evictions")
    second = nil
    expect(weakSecond == nil, "listener that returned false was detached and freed")
    expect(try store.delete(key: "three"), "delete three after detach")

    // A listener that throws surfaces to the caller as the foreign error
    // (code -4) carrying the thrown description; the eviction itself already
    // happened, and the store stays usable.
    let thrower = Listener(failKey: "boom")
    store.setEvictionListener(listener: thrower)
    _ = try store.put(key: "boom", value: payload, kind: .volatile, ttlSeconds: nil)
    _ = try store.put(key: "fine", value: payload, kind: .volatile, ttlSeconds: nil)
    do {
        _ = try store.delete(key: "boom")
        fail("expected a foreign error from the throwing listener")
    } catch let e as WeaveFFIError {
        expect(e.errorCode == -4, "foreign error code == -4 (got \(e.errorCode))")
        expect(e.localizedDescription.contains("listener rejected boom"),
               "foreign error carries the thrown message (got \(e.localizedDescription))")
    }
    expect(store.count() == 2, "boom was removed before the listener ran")
    expect(try store.delete(key: "fine"), "store usable after a foreign error")
    expect(thrower.evicted.map { $0.key } == ["fine"], "non-failing eviction still observed")
    // An expired read that trips the listener also surfaces as -4 rather
    // than the domain code.
    _ = try store.put(key: "boom", value: payload, kind: .volatile, ttlSeconds: -1)
    do {
        _ = try store.get(key: "boom")
        fail("expected a foreign error from the throwing listener on expiry")
    } catch let e as WeaveFFIError {
        expect(e.errorCode == -4, "expiry foreign error code == -4 (got \(e.errorCode))")
    }
    store.clearEvictionListener()
    _ = try store.put(key: "boom", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(try store.delete(key: "boom"), "delete boom with no listener attached")
    expect(thrower.evicted.count == 1, "cleared listener no longer notified")
    store.clearEvictionListener()

    // --- Object graph: share, fork, larger, describe, openMany, totalCount ---
    expect(store.count() == 1, "only alpha remains (got \(store.count()))")

    // share(): a second wrapper to the SAME object; mutation through one is
    // visible through the other, and dropping one leaves the other usable.
    var shared: Store? = store.share()
    weak var weakShared = shared
    _ = try shared!.put(key: "via-share", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(store.count() == 2, "put through share() visible through the original")
    expect(Array(try shared!.listKeys(prefix: nil)) == ["alpha", "via-share"], "share() reads the same entries")
    shared = nil
    expect(weakShared == nil, "shared wrapper deinitialized")
    expect(store.count() == 2, "original still alive after releasing the shared wrapper")

    // fork(): an independent copy.
    let forked = store.fork()
    expect(forked.count() == 2, "fork copies live entries")
    _ = try forked.put(key: "fork-only", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(forked.count() == 3 && store.count() == 2, "fork is independent of the original")
    do {
        _ = try store.get(key: "fork-only")
        fail("expected KvError.keyNotFound: a fork's insert must not reach the original")
    } catch let e as KvError {
        guard case .keyNotFound = e else { fail("expected .keyNotFound from the fork probe, got \(e)") }
    }
} catch {
    fail("threw: \(error)")
}

do {
    let payload = Data([1, 2, 3])
    let small = try Store.open(path: "/tmp/small")
    let big = try Store.open(path: "/tmp/big")
    _ = try big.put(key: "k", value: payload, kind: .volatile, ttlSeconds: nil)

    // larger(): Store? as parameter and return.
    expect(small.larger(other: nil) == nil, "larger(nil) on an empty store is nil")
    let bigger = small.larger(other: big)
    expect(bigger != nil && bigger!.count() == 1, "larger picks the fuller store")
    _ = try bigger!.put(key: "k2", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(big.count() == 2, "the returned wrapper is the same object as `big`")
    let own = big.larger(other: nil)
    expect(own != nil && own!.count() == 2, "larger(nil) on a non-empty store returns itself")
    _ = try own!.put(key: "k3", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(big.count() == 3, "self returned from larger(nil) is the same object")
    expect(small.larger(other: small)?.count() == 0, "larger(self) returns a store")

    // describe(): a record carrying an object field and an optional object.
    let info = big.describe(label: "primary", mirror: nil)
    expect(info.label == "primary", "describe label")
    expect(info.count == 3, "describe count (got \(info.count))")
    expect(info.mirror == nil, "describe mirror absent")
    expect(info.store.count() == 3, "describe().store is usable")
    _ = try info.store.put(key: "k4", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(big.count() == 4, "describe().store is the same object as the receiver")
    let mirrored = big.describe(label: "with-mirror", mirror: small)
    expect(mirrored.mirror != nil && mirrored.mirror!.count() == 0, "describe mirror present")
    _ = try mirrored.mirror!.put(key: "s", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(small.count() == 1, "describe().mirror is the same object as the argument")

    // openMany(): a list of objects as a return; each is a distinct store.
    let many = try Store.openMany(paths: ["/a", "/b", "/c"])
    expect(many.count == 3, "openMany returns three stores (got \(many.count))")
    _ = try many[0].put(key: "m", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(many[0].count() == 1 && many[1].count() == 0 && many[2].count() == 0,
           "openMany stores are independent")
    do {
        _ = try Store.openMany(paths: ["/ok", ""])
        fail("expected KvError.ioError from openMany with an empty path")
    } catch let e as KvError {
        guard case .ioError = e else { fail("expected .ioError, got \(e)") }
        expect(e.errorCode == 1004, "openMany ioError code == 1004")
    }

    // totalCount(): objects inside a list parameter and inside an optional
    // record parameter. Every object is cloned into the buffer, so the
    // wrappers stay valid afterwards.
    expect(Store.totalCount(stores: many, extra: nil) == 1, "totalCount over the list only")
    expect(Store.totalCount(stores: [big, small, many[0]], extra: nil) == 6, "totalCount mixed (4 + 1 + 1)")
    let extra = StoreInfo(label: "extra", store: big, mirror: small, count: -1)
    expect(Store.totalCount(stores: many, extra: extra) == 5, "totalCount adds extra.store (1 + 4)")
    expect(Store.totalCount(stores: [], extra: info) == 4, "totalCount with an empty list and a record")
    expect(Store.totalCount(stores: [], extra: nil) == 0, "totalCount of nothing")
    expect(big.count() == 4 && small.count() == 1 && many[0].count() == 1,
           "every store is still usable after being encoded into buffers")

    // Wrappers release through deinit; a weak reference proves it ran.
    weak var weakTmp: Store?
    do {
        let tmp = try Store.open(path: "/tmp/tmp")
        weakTmp = tmp
        expect(tmp.count() == 0, "tmp usable")
    }
    expect(weakTmp == nil, "temporary store wrapper deinitialized")
    // A wrapper adopted from a record keeps the object alive after the
    // wrapper it came from is gone.
    var owner: Store? = try Store.open(path: "/tmp/owner")
    _ = try owner!.put(key: "x", value: payload, kind: .volatile, ttlSeconds: nil)
    let ownerInfo = owner!.describe(label: "o", mirror: nil)
    owner = nil
    expect(ownerInfo.store.count() == 1, "object outlives the wrapper that described it")

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    // the continuation resumes from the producer's worker thread.
    _ = try big.put(key: "doomed", value: payload, kind: .volatile, ttlSeconds: 0)
    expect(try Kv.Stats.getStats(store: big).expiredEntries == 1, "stats counts the doomed entry as expired")
    let reclaimed = try await big.compact()
    expect(reclaimed == 3, "compact reclaimed 3 bytes (got \(reclaimed))")
    expect(big.count() == 4, "compact dropped only the expired entry")
    expect(try await small.compact() == 0, "compact with nothing expired reclaims 0")

    // Non-throwing void method plus the static, neither needing `try`.
    big.clear()
    expect(big.count() == 0, "clear drops everything")
    expect(Store.defaultCapacity() == 1_000_000, "defaultCapacity")

    // The throwing constructor rejects an empty path with ioError (1004).
    do {
        _ = try Store.open(path: "")
        fail("expected KvError.ioError for empty path")
    } catch let e as KvError {
        guard case let .ioError(message) = e else { fail("expected .ioError, got \(e)") }
        expect(e.errorCode == 1004, "ioError code == 1004 (got \(e.errorCode))")
        expect(message == "I/O failure", "ioError message (got \(message))")
    }

    // No explicit close: every Store deinit calls the generated destroy.
    print("swift/kvstore: OK")
} catch {
    fail("threw: \(error)")
}
