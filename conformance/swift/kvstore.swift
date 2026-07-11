// Conformance consumer: kvstore sample, Swift target.
//
// Binds through the generated `Kvstore` module and exercises the 0.5.0
// interface surface: `Store` as a final class opened via the throwing static
// factory `Store.open(path:)`, throwing methods raising the typed `KvError`
// domain enum (put/get/delete/listKeys), non-throwing methods without `try`
// (count/clear), the static `Store.defaultCapacity()`, the builder's list/map
// input marshaling, the map return-by-value getter (`entry.metadata`), the
// iterator lowering behind `listKeys`, the nested `Kv.Stats` submodule, the
// context-boxed eviction listener, and the CheckedContinuation-backed
// `compact()` async method (top-level await; resumed from the producer's
// worker thread). Typed-error asserts pin the case and the numeric code
// carried by `errorCode` (keyNotFound 1001, expired 1002, ioError 1004).

import Foundation
import Kvstore

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

do {
    let store = try Store.open(path: "/tmp/conformance-kvstore-swift")

    let payload = Data([1, 2, 3])
    _ = try store.put(key: "alpha", value: payload, kind: .persistent, ttlSeconds: nil)
    _ = try store.put(key: "beta", value: payload, kind: .volatile, ttlSeconds: nil)
    expect(store.count() == 2, "count == 2")

    // A present key round-trips through the optional Entry return.
    let fetched = try store.get(key: "alpha")
    expect(fetched?.key == "alpha", "get alpha key")
    expect(fetched?.value == payload, "get alpha value")

    // Iterator lowering: a lazy single-pass Sequence pulled one element per
    // step, drained here into [String] (the BTreeMap's sorted order). A
    // per-next producer error would end iteration and set `.error`.
    let keysIter = try store.listKeys(prefix: nil)
    let keys = Array(keysIter)
    expect(keysIter.error == nil, "listKeys iteration error-free")
    expect(keys == ["alpha", "beta"], "listKeys sorted (got \(keys))")
    expect(Array(try store.listKeys(prefix: "al")) == ["alpha"], "listKeys prefix filter")

    // A missing key raises the typed domain error's keyNotFound case (1001).
    do {
        _ = try store.get(key: "missing")
        fail("expected KvError.keyNotFound for missing key")
    } catch let e as KvError {
        guard case .keyNotFound = e else { fail("expected .keyNotFound, got \(e)") }
        expect(e.errorCode == 1001, "keyNotFound code == 1001 (got \(e.errorCode))")
    }

    // Builder marshals a [String] list and a [String: String] map in.
    let entry = try EntryBuilder()
        .withId(7)
        .withKey("alpha")
        .withValue(payload)
        .withCreatedAt(1000)
        .withExpiresAt(nil)
        .withTags(["hot", "fast"])
        .withMetadata(["source": "test", "env": "prod"])
        .build()
    expect(entry.id == 7, "entry id")

    // List getter.
    let tags = entry.tags
    expect(tags.count == 2 && tags.contains("hot") && tags.contains("fast"), "tags")

    // Map getter over the triple-pointer out-params.
    let md = entry.metadata
    expect(md.count == 2, "metadata count")
    expect(md["source"] == "test", "metadata source")
    expect(md["env"] == "prod", "metadata env")

    // Empty map round-trips as an empty dictionary.
    let empty = try EntryBuilder()
        .withId(8)
        .withKey("k")
        .withValue(payload)
        .withCreatedAt(1)
        .withExpiresAt(nil)
        .withTags([])
        .withMetadata([:])
        .build()
    expect(empty.metadata.isEmpty, "empty metadata")

    // Nested Kv.Stats submodule (name collides with the module-level Stats),
    // passing the Store interface across the module boundary.
    let stats = try Kv.Stats.getStats(store: store)
    expect(stats.total_entries == 2, "stats total_entries")
    expect(stats.total_bytes == 6, "stats total_bytes")

    // Eviction listener: delete fires the context-boxed trampoline
    // synchronously on the calling thread.
    final class Recorder { var evicted: [String] = [] }
    let recorder = Recorder()
    let sub = Kv.registerEvictionListener { key in recorder.evicted.append(key) }
    expect(sub > 0, "listener id positive")
    expect(try store.delete(key: "beta"), "delete beta")
    expect(recorder.evicted == ["beta"], "eviction fired for beta (got \(recorder.evicted))")

    // Unregister stops delivery.
    Kv.unregisterEvictionListener(sub)
    expect(try store.delete(key: "alpha"), "delete alpha")
    expect(recorder.evicted == ["beta"], "no eviction after unregister (got \(recorder.evicted))")

    // TTL expiry: reading an already-expired entry raises the expired case
    // (1002) and evicts it (silently; the listener is unregistered).
    _ = try store.put(key: "ttl", value: payload, kind: .volatile, ttlSeconds: -1)
    do {
        _ = try store.get(key: "ttl")
        fail("expected KvError.expired for expired entry")
    } catch let e as KvError {
        guard case .expired = e else { fail("expected .expired, got \(e)") }
        expect(e.errorCode == 1002, "expired code == 1002 (got \(e.errorCode))")
    }
    expect(recorder.evicted == ["beta"], "expiry eviction not delivered after unregister")

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    // the continuation resumes from the producer's worker thread.
    _ = try store.put(key: "doomed", value: payload, kind: .volatile, ttlSeconds: 0)
    let reclaimed = try await store.compact()
    expect(reclaimed == 3, "compact reclaimed 3 bytes (got \(reclaimed))")
    expect(store.count() == 0, "store empty after deletes + compact")

    // Non-throwing void method plus the static, neither needing `try`.
    _ = try store.put(key: "gone", value: payload, kind: .volatile, ttlSeconds: nil)
    store.clear()
    expect(store.count() == 0, "clear drops everything")
    expect(Store.defaultCapacity() == 1_000_000, "defaultCapacity")

    // The throwing constructor rejects an empty path with ioError (1004).
    do {
        _ = try Store.open(path: "")
        fail("expected KvError.ioError for empty path")
    } catch let e as KvError {
        guard case .ioError = e else { fail("expected .ioError, got \(e)") }
        expect(e.errorCode == 1004, "ioError code == 1004 (got \(e.errorCode))")
    }

    // No explicit close: the Store deinit calls the generated destroy symbol.
    print("swift/kvstore: OK")
} catch {
    fail("threw: \(error)")
}
