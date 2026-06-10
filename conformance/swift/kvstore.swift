// Conformance consumer: kvstore sample, Swift target.
//
// Binds through the generated `Kvstore` module and exercises the parts the
// Swift backend previously generated incorrectly: the map return-by-value
// getter (`entry.metadata` over the triple-pointer out-params), the `[String]`
// list getter, the builder's list/map *input* marshaling (strdup + free), the
// iterator `next` out-param convention (`kv_list_keys`), and the nested
// `Kv.Stats` submodule whose `Stats` collides with the module namespace.
// Also covers the context-boxed eviction listener (register -> fire
// synchronously on delete -> unregister) and the CheckedContinuation-backed
// `kv_compact_async` (top-level await; resumed from the producer's worker
// thread).

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
    let store = try Kv.kv_open_store("/tmp/conformance-kvstore-swift")

    let payload = Data([1, 2, 3])
    _ = try Kv.kv_put(store, "alpha", payload, .persistent, nil)
    _ = try Kv.kv_put(store, "beta", payload, .volatile, nil)
    expect(try Kv.kv_count(store) == 2, "count == 2")

    // Iterator `next` out-param convention, materialized to [String].
    let keys = try Kv.kv_list_keys(store, nil)
    expect(keys.count == 2, "list_keys count")
    expect(keys.contains("alpha") && keys.contains("beta"), "list_keys values")

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

    // Map getter over the triple-pointer out-params (the redesign).
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

    // Nested Kv.Stats submodule (name collides with the module-level Stats).
    let stats = try Kv.Stats.kv_stats_get_stats(store)
    expect(stats.total_entries == 2, "stats total_entries")

    // Eviction listener: delete fires the context-boxed trampoline
    // synchronously on the calling thread.
    final class Recorder { var evicted: [String] = [] }
    let recorder = Recorder()
    let sub = Kv.kv_register_eviction_listener { key in recorder.evicted.append(key) }
    expect(sub > 0, "listener id positive")
    expect(try Kv.kv_delete(store, "beta"), "delete beta")
    expect(recorder.evicted == ["beta"], "eviction fired for beta (got \(recorder.evicted))")

    // Unregister stops delivery.
    Kv.kv_unregister_eviction_listener(sub)
    expect(try Kv.kv_delete(store, "alpha"), "delete alpha")
    expect(recorder.evicted == ["beta"], "no eviction after unregister (got \(recorder.evicted))")

    // Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    // the continuation resumes from the producer's worker thread.
    _ = try Kv.kv_put(store, "doomed", payload, .volatile, 0)
    let reclaimed = try await Kv.kv_compact_async(store)
    expect(reclaimed == 3, "compact reclaimed 3 bytes (got \(reclaimed))")
    expect(try Kv.kv_count(store) == 0, "store empty after deletes + compact")

    // No explicit kv_close_store: the producer frees the store there *and* the
    // Store deinit calls Store_destroy, so closing here would double-free.
    print("swift/kvstore: OK")
} catch {
    fail("threw: \(error)")
}
