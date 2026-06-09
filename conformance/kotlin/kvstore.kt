// Conformance consumer: kvstore sample, Android/Kotlin (JNI) target.
//
// Exercises the struct-materialization paths the JNI layer previously stubbed or
// mis-marshalled: the `ByteArray` bytes getter (`Entry.value`), the `Long?`
// nullable-scalar getter (`Entry.expiresAt`), the `Array<String>` list getter
// (`Entry.tags`) and the `Map<String,String>` map getter (`Entry.metadata`) over
// the triple-pointer ABI. Also covers the iterator-backed `kv_list_keys`
// (drained into a Kotlin `Iterator`), the typed-handle return of `kv_open_store`
// (re-wrapped into `Store`), the `EntryBuilder` (optional fields pass through),
// and the `kv.stats` submodule. Compiled in-module with the generated
// `WeaveFFI.kt`, so the `internal` `Entry`/`Stats` constructors are reachable.
@file:JvmName("Main")

import com.weaveffi.Entry
import com.weaveffi.EntryBuilder
import com.weaveffi.EntryKind
import com.weaveffi.Stats
import com.weaveffi.WeaveFFI
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

fun main() {
    val store = WeaveFFI.kv_open_store("/tmp/conformance-kvstore-kotlin")
    val payload = byteArrayOf(1, 2, 3)
    expect(WeaveFFI.kv_put(store, "alpha", payload, EntryKind.Persistent, null), "put alpha")
    expect(WeaveFFI.kv_put(store, "beta", payload, EntryKind.Volatile, 3600L), "put beta with ttl")
    expect(WeaveFFI.kv_count(store) == 2L, "count == 2")

    // Iterator-backed list-of-string return, drained through Kotlin's Iterator.
    val keys = mutableListOf<String>()
    val it = WeaveFFI.kv_list_keys(store, null)
    while (it.hasNext()) keys.add(it.next())
    keys.sort()
    expect(keys == listOf("alpha", "beta"), "list_keys values")

    // Optional struct return -> nullable raw handle -> wrap in Entry.
    val alphaHandle = WeaveFFI.kv_get(store, "alpha")
    expect(alphaHandle != null, "get alpha present")
    val alpha = Entry(alphaHandle!!)
    expect(alpha.id > 0, "entry id positive")
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

    val beta = Entry(WeaveFFI.kv_get(store, "beta")!!)
    expect(beta.expires_at != null && beta.expires_at!! > 0L, "beta expires_at present")

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

    // kv.stats submodule.
    val stats = Stats(WeaveFFI.kv_stats_get_stats(store))
    expect(stats.total_entries == 2L, "stats total entries == 2")

    println("kotlin/kvstore: OK")
}
