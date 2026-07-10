// Conformance consumer: kvstore sample, Android/Kotlin (JNI) target.
//
// Exercises the 0.5.0 interface surface: `Store` is a generated Closeable
// class (companion factory `open`, instance methods, static `defaultCapacity`,
// destroy through `close()`), and `KvError` is a typed exception domain
// (`KvException` sealed subclasses extending the generic `WeaveFFIException`).
// Asserts the typed-error paths (IoError from `open("")`, KeyNotFound from a
// missing `get`, Expired from a TTL-elapsed `get`), plus the existing
// behavioral surface adapted to the class API: struct materialization
// (`Entry.value` bytes, nullable `expires_at`, `tags` array, `metadata` map
// over the triple-pointer ABI), the iterator-backed `listKeys` with prefix
// filtering, the `EntryBuilder` round-trip, the nested `kv.stats` module, the
// JNI eviction listener (register, fire, unregister), the deprecated
// `legacyPut`, and the suspend `compact` resumed from the producer's worker
// thread. Compiled in-module with the generated `WeaveFFI.kt`, so `internal`
// constructors are reachable.
@file:JvmName("Main")

import com.weaveffi.EntryBuilder
import com.weaveffi.EntryKind
import com.weaveffi.KvException
import com.weaveffi.Store
import com.weaveffi.WeaveFFI
import com.weaveffi.WeaveFFIException
import kotlin.system.exitProcess
import kotlinx.coroutines.runBlocking

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

/** Run `block` and return the exception it threw, or null if it completed. */
inline fun thrownBy(block: () -> Unit): Throwable? =
    try {
        block()
        null
    } catch (e: Throwable) {
        e
    }

fun main() {
    // Typed error from a constructor: an empty path is rejected with the
    // domain's IoError (1004), which is both the sealed domain type and the
    // generic brand exception.
    val openErr = thrownBy { Store.open("") }
    expect(openErr is KvException.IoError, "open(\"\") throws KvException.IoError (got $openErr)")
    expect(openErr is KvException, "IoError is a KvException")
    expect(openErr is WeaveFFIException, "IoError is a WeaveFFIException")
    val openCode = (openErr as? WeaveFFIException)?.code
    expect(openCode == 1004, "IoError code 1004 (got $openCode)")

    // Static on the interface's companion.
    expect(Store.defaultCapacity() == 1_000_000L, "defaultCapacity == 1000000")

    Store.open("/tmp/conformance-kvstore-kotlin").use { store ->
        val payload = byteArrayOf(1, 2, 3)
        expect(store.put("alpha", payload, EntryKind.Persistent, null), "put alpha")
        expect(store.put("beta", payload, EntryKind.Volatile, 3600L), "put beta with ttl")
        expect(store.count() == 2L, "count == 2")

        // Deprecated method still round-trips (volatile, no TTL).
        @Suppress("DEPRECATION")
        val legacyOk = store.legacyPut("legacy", payload)
        expect(legacyOk, "legacyPut inserts")
        expect(store.count() == 3L, "count == 3 after legacyPut")
        expect(store.delete("legacy"), "delete legacy")

        // Iterator-backed list-of-string return, drained through Kotlin's
        // Iterator; the backing BTreeMap yields sorted order.
        val keys = mutableListOf<String>()
        val it = store.listKeys(null)
        while (it.hasNext()) keys.add(it.next())
        expect(keys == listOf("alpha", "beta"), "listKeys sorted (got $keys)")

        // Optional prefix filter.
        val filtered = mutableListOf<String>()
        val itAl = store.listKeys("al")
        while (itAl.hasNext()) filtered.add(itAl.next())
        expect(filtered == listOf("alpha"), "listKeys prefix filter (got $filtered)")

        // Optional struct return arrives as a wrapped `Entry?`.
        val alpha = store.get("alpha")
        expect(alpha != null, "get alpha present")
        expect(alpha!!.id > 0, "entry id positive")
        expect(alpha.key == "alpha", "entry key")

        // Bytes getter -> ByteArray.
        expect(
            alpha.value.size == 3 && alpha.value[0].toInt() == 1 && alpha.value[2].toInt() == 3,
            "entry value bytes"
        )
        // Optional-scalar getter: alpha had no TTL -> null.
        expect(alpha.expires_at == null, "alpha expires_at null")
        // `put` stores empty tags/metadata, so the getters return empty collections.
        expect(alpha.tags.isEmpty(), "alpha tags empty")
        expect(alpha.metadata.isEmpty(), "alpha metadata empty")

        val beta = store.get("beta")
        expect(beta != null && beta.expires_at != null && beta.expires_at!! > 0L, "beta expires_at present")

        // Typed error from a method: a missing key reports KeyNotFound (1001).
        val missingErr = thrownBy { store.get("missing") }
        expect(
            missingErr is KvException.KeyNotFound,
            "get(missing) throws KvException.KeyNotFound (got $missingErr)"
        )
        val missingCode = (missingErr as? WeaveFFIException)?.code
        expect(missingCode == 1001, "KeyNotFound code 1001 (got $missingCode)")

        // TTL expiry: a zero-TTL entry is already expired, so `get` reports
        // Expired (1002) and evicts the entry on read.
        expect(store.put("ephemeral", payload, EntryKind.Volatile, 0L), "put ephemeral")
        val expiredErr = thrownBy { store.get("ephemeral") }
        expect(
            expiredErr is KvException.Expired,
            "get(expired) throws KvException.Expired (got $expiredErr)"
        )
        expect(store.count() == 2L, "expired entry evicted on read")

        // Builder carries a non-empty list + map so the list/map getters return
        // producer-allocated arrays (the case the triple-pointer ABI redesign fixes).
        val built = EntryBuilder()
            .withId(7L)
            .withKey("built")
            .withValue(payload)
            .withCreatedAt(1000L)
            .withExpiresAt(null)
            .withTags(arrayOf("hot", "fast"))
            .withMetadata(mapOf("source" to "test", "env" to "prod"))
            .build()
        expect(built.tags.toSet() == setOf("hot", "fast"), "built tags")
        expect(
            built.metadata["source"] == "test" && built.metadata["env"] == "prod",
            "built metadata"
        )
        // Optional field omitted (null) round-trips as absent, not an error.
        expect(built.expires_at == null, "built expires_at null")

        // Empty collections via the builder still decode cleanly.
        val empty = EntryBuilder()
            .withId(8L)
            .withKey("empty")
            .withValue(payload)
            .withCreatedAt(1L)
            .withExpiresAt(99L)
            .withTags(arrayOf())
            .withMetadata(emptyMap())
            .build()
        expect(empty.tags.isEmpty(), "empty tags")
        expect(empty.metadata.isEmpty(), "empty metadata")
        expect(empty.expires_at == 99L, "empty expires_at present")

        // kv.stats submodule: free function taking the interface (borrowed
        // handle) and returning a wrapped struct.
        val stats = WeaveFFI.getStats(store)
        expect(stats.total_entries == 2L, "stats total entries == 2")
        expect(stats.total_bytes == 6L, "stats total bytes == 6 (got ${stats.total_bytes})")

        // Eviction listener: deleting an existing key fires OnEvict synchronously
        // through the JNI trampoline (producer thread == caller thread here).
        val evicted = mutableListOf<String>()
        val sub = WeaveFFI.registerEvictionListener { key -> evicted.add(key) }
        expect(sub > 0L, "listener id positive")
        expect(store.delete("beta"), "delete beta")
        expect(evicted == listOf("beta"), "eviction fired for beta (got $evicted)")

        // Unregister stops delivery.
        WeaveFFI.unregisterEvictionListener(sub)
        expect(store.delete("alpha"), "delete alpha")
        expect(evicted == listOf("beta"), "no eviction after unregister (got $evicted)")

        // Suspend async: an immediately-expired entry gives compact 3 bytes to
        // reclaim; the continuation resumes from the producer's worker thread.
        expect(store.put("doomed", payload, EntryKind.Volatile, 0L), "put doomed")
        val reclaimed = runBlocking { store.compact() }
        expect(reclaimed == 3L, "compact reclaimed 3 bytes (got $reclaimed)")
        expect(store.count() == 0L, "store empty after deletes + compact")

        // clear() drops everything that remains.
        expect(store.put("last", payload, EntryKind.Persistent, null), "put last")
        store.clear()
        expect(store.count() == 0L, "store empty after clear")
    }

    println("kotlin/kvstore: OK")
}
