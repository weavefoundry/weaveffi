// Conformance consumer: kvstore sample, Kotlin (JVM via JNI) target.
//
// Exercises the reference-counted interface surface: `Store` is a generated
// `AutoCloseable` class (companion factory `open`, instance methods, statics
// `defaultCapacity`/`openMany`/`totalCount`, `close()` releases one strong
// reference and the `Cleaner` is the backstop), `Entry`, `Stats`, and
// `StoreInfo` are data classes decoded from value buffers (`StoreInfo` carries
// a `Store` object and a nullable one as fields), and `KvError` is a typed
// exception domain (`KvException` sealed subclasses extending the generic
// `WeaveFFIException`). Asserts the typed-error paths (IoError from
// `open("")` and `openMany`, KeyNotFound, Expired), the ABI 2 object graph
// (`share()` returns a wrapper over the same object so writes through one are
// visible through the other, `fork()` is independent, `larger(null)`,
// `describe().store`, `openMany`, `totalCount` with objects inside lists and
// records), the consumer-implemented `EvictionListener` callback interface
// (every eviction arrives with the decoded `Entry` and the right
// `EvictionReason`; returning false detaches; replacing or clearing releases
// the previous listener; a Kotlin exception thrown inside `onEvict` surfaces
// to the caller as `WeaveFFIException` code -4 without crashing the JVM),
// plus the existing surface: record materialization, buffered optional
// parameters, the iterator-backed `listKeys`, the `Entry` pack/unpack round
// trip, the nested `kv.stats` module, the deprecated `legacyPut`, the suspend
// `compact` driven with `runBlocking`, and close semantics (double `close()`
// safe, use after close throws). Compiled in-module with the generated
// `WeaveFFI.kt`, so the `internal` helpers and `handle` are reachable.
@file:JvmName("Main")

import com.weaveffi.Entry
import com.weaveffi.EntryKind
import com.weaveffi.EvictionListener
import com.weaveffi.EvictionReason
import com.weaveffi.KvException
import com.weaveffi.Store
import com.weaveffi.StoreInfo
import com.weaveffi.WeaveFFI
import com.weaveffi.WeaveFFIException
import com.weaveffi.packEntry
import com.weaveffi.unpackEntry
import com.weaveffi.weaveDecode
import com.weaveffi.weaveEncode
import java.lang.ref.WeakReference
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

/** Spin the collector until `ref` clears or we give up. */
fun collected(ref: WeakReference<*>): Boolean {
    for (i in 0 until 200) {
        if (ref.get() == null) return true
        System.gc()
        System.runFinalization()
        Thread.sleep(5)
    }
    return ref.get() == null
}

/**
 * A listener that records every eviction. It detaches itself (returns false)
 * once it has seen `keepAfter` evictions, and throws when the evicted key is
 * `failOnKey`.
 */
class RecordingListener(
    val name: String,
    private val keepAfter: Int = Int.MAX_VALUE,
    private val failOnKey: String? = null,
) : EvictionListener {
    val evictions = mutableListOf<Pair<String, EvictionReason>>()
    val entries = mutableListOf<Entry>()

    override fun onEvict(entry: Entry, reason: EvictionReason): Boolean {
        if (entry.key == failOnKey) throw IllegalStateException("listener $name refused ${entry.key}")
        evictions.add(entry.key to reason)
        entries.add(entry)
        return evictions.size < keepAfter
    }
}

/** Attach a listener nothing else references, returning a weak handle to it. */
fun attachThrowaway(store: Store, name: String): WeakReference<RecordingListener> {
    val listener = RecordingListener(name)
    store.setEvictionListener(listener)
    return WeakReference(listener)
}

/** Attach a self-detaching listener and drive one eviction, returning a weak handle to it. */
fun attachSelfDetaching(store: Store, payload: ByteArray): WeakReference<RecordingListener> {
    val listener = RecordingListener("once", keepAfter = 1)
    store.setEvictionListener(listener)
    expect(store.put("first", payload, EntryKind.Volatile, null), "put first")
    expect(store.put("second", payload, EntryKind.Volatile, null), "put second")
    expect(store.delete("first"), "delete first")
    expect(listener.evictions == listOf("first" to EvictionReason.Deleted), "self-detaching listener saw first")
    // It returned false, so the store dropped it; the next eviction isn't seen.
    expect(store.delete("second"), "delete second")
    expect(listener.evictions.size == 1, "detached listener saw nothing more (got ${listener.evictions})")
    return WeakReference(listener)
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
    expect(openErr?.message == "I/O failure", "IoError message (got ${openErr?.message})")

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
        expect(!store.delete("legacy"), "second delete reports false")

        // Iterator-backed list-of-string return, drained through Kotlin's
        // Iterator and as a Sequence; the backing BTreeMap yields sorted
        // order. The absent prefix crosses as a buffered `string?`.
        val keys = mutableListOf<String>()
        val it = store.listKeys(null)
        while (it.hasNext()) keys.add(it.next())
        expect(keys == listOf("alpha", "beta"), "listKeys sorted (got $keys)")
        expect(thrownBy { it.next() } is NoSuchElementException, "exhausted iterator throws")
        expect(store.listKeys("al").asSequence().toList() == listOf("alpha"), "listKeys prefix filter")
        expect(store.listKeys("zz").asSequence().toList().isEmpty(), "listKeys unmatched prefix")

        // Buffered `Entry?` return, decoded into the data class.
        val alpha = store.get("alpha")
        expect(alpha != null, "get alpha present")
        expect(alpha!!.id > 0, "entry id positive")
        expect(alpha.key == "alpha", "entry key")
        expect(alpha.value.contentEquals(payload), "entry value bytes")
        expect(alpha.expires_at == null, "alpha expires_at null")
        expect(alpha.tags.isEmpty(), "alpha tags empty")
        expect(alpha.metadata.isEmpty(), "alpha metadata empty")

        val beta = store.get("beta")
        expect(beta != null && beta.expires_at != null && beta.expires_at > beta.created_at, "beta expires_at present")
        expect(beta!!.id == alpha.id + 1, "ids are monotonic (got ${alpha.id}, ${beta.id})")

        // Typed error from a method: a missing key reports KeyNotFound (1001).
        val missingErr = thrownBy { store.get("missing") }
        expect(missingErr is KvException.KeyNotFound, "get(missing) throws KvException.KeyNotFound (got $missingErr)")
        expect((missingErr as WeaveFFIException).code == 1001, "KeyNotFound code 1001 (got ${missingErr.code})")
        expect(missingErr.message == "key not found", "KeyNotFound message (got ${missingErr.message})")

        // TTL expiry: a zero-TTL entry is already expired, so `get` reports
        // Expired (1002) and evicts the entry on read.
        expect(store.put("ephemeral", payload, EntryKind.Volatile, 0L), "put ephemeral")
        val expiredErr = thrownBy { store.get("ephemeral") }
        expect(expiredErr is KvException.Expired, "get(expired) throws KvException.Expired (got $expiredErr)")
        expect((expiredErr as WeaveFFIException).code == 1002, "Expired code 1002")
        expect(store.count() == 2L, "expired entry evicted on read")

        // An Entry with a non-empty list + map round-trips through the
        // generated pack/unpack routines.
        val built = Entry(
            id = 7L,
            key = "built",
            value = payload,
            created_at = 1000L,
            expires_at = null,
            tags = listOf("hot", "fast"),
            metadata = mapOf("source" to "test", "env" to "prod"),
        )
        val builtBack = weaveDecode(weaveEncode { w -> packEntry(w, built) }) { r -> unpackEntry(r) }
        expect(builtBack.tags == listOf("hot", "fast"), "built tags")
        expect(builtBack.metadata == mapOf("source" to "test", "env" to "prod"), "built metadata")
        expect(builtBack.value.contentEquals(payload), "built value bytes")
        expect(builtBack.expires_at == null, "built expires_at null")

        val empty = Entry(8L, "empty", payload, 1L, 99L, listOf(), emptyMap())
        val emptyBack = weaveDecode(weaveEncode { w -> packEntry(w, empty) }) { r -> unpackEntry(r) }
        expect(emptyBack.tags.isEmpty() && emptyBack.metadata.isEmpty(), "empty collections")
        expect(emptyBack.expires_at == 99L, "empty expires_at present")

        // kv.stats submodule: free function taking the interface (borrowed
        // handle) and returning a buffered record.
        val stats = WeaveFFI.getStats(store)
        expect(stats.total_entries == 2L, "stats total entries == 2")
        expect(stats.total_bytes == 6L, "stats total bytes == 6 (got ${stats.total_bytes})")
        expect(stats.expired_entries == 0L, "stats expired == 0")

        // --- Callback interface: EvictionListener -------------------------

        // Deleting an existing key fires onEvict synchronously with the
        // decoded Entry and reason Deleted; a TTL-expired read fires Expired.
        val listener = RecordingListener("main")
        store.setEvictionListener(listener)
        expect(store.delete("beta"), "delete beta")
        expect(listener.evictions == listOf("beta" to EvictionReason.Deleted), "eviction for beta (got ${listener.evictions})")
        expect(listener.entries[0].id == beta.id, "evicted entry carries beta's id")
        expect(listener.entries[0].value.contentEquals(payload), "evicted entry carries beta's bytes")
        expect(listener.entries[0].expires_at == beta.expires_at, "evicted entry carries beta's expiry")
        expect(!store.delete("beta"), "delete of a missing key")
        expect(listener.evictions.size == 1, "missing key evicts nothing")
        expect(store.put("gone", payload, EntryKind.Volatile, 0L), "put gone with zero TTL")
        expect(thrownBy { store.get("gone") } is KvException.Expired, "gone is expired")
        expect(
            listener.evictions == listOf("beta" to EvictionReason.Deleted, "gone" to EvictionReason.Expired),
            "expiry eviction (got ${listener.evictions})"
        )
        // compact does not notify (it reclaims without going through the
        // listener), so listener state is unchanged afterwards.
        expect(store.put("doomed", payload, EntryKind.Volatile, 0L), "put doomed")
        val reclaimed = runBlocking { store.compact() }
        expect(reclaimed == 3L, "compact reclaimed 3 bytes (got $reclaimed)")
        expect(listener.evictions.size == 2, "compact bypasses the listener")

        // Replacing the listener releases the previous one (the producer's
        // `free` drops the GlobalRef, so it becomes collectable), as does
        // clearing it.
        val first = attachThrowaway(store, "first")
        System.gc()
        expect(first.get() != null, "attached listener stays pinned by the producer")
        val second = attachThrowaway(store, "second")
        expect(collected(first), "replaced listener released")
        expect(second.get() != null, "current listener still pinned")
        store.clearEvictionListener()
        expect(collected(second), "cleared listener released")
        expect(store.delete("alpha"), "delete alpha with no listener")
        expect(listener.evictions.size == 2, "detached listener sees nothing")

        // A listener returning false is detached by the store and released.
        expect(collected(attachSelfDetaching(store, payload)), "self-detached listener released")
        expect(store.count() == 0L, "store empty after self-detach test")

        // A Kotlin exception thrown from onEvict surfaces to the caller as
        // WeaveFFIException(-4), not as a KvException, even though `delete`
        // has the KvError domain; the JVM keeps running, the entry is gone
        // (the producer removes before notifying), and the listener stays
        // attached.
        val thrower = RecordingListener("thrower", failOnKey = "boom")
        store.setEvictionListener(thrower)
        expect(store.put("boom", payload, EntryKind.Persistent, null), "put boom")
        val foreign = thrownBy { store.delete("boom") }
        expect(foreign is WeaveFFIException, "throwing onEvict surfaces as WeaveFFIException (got $foreign)")
        expect(foreign !is KvException, "foreign error is not a domain error")
        expect((foreign as WeaveFFIException).code == -4, "foreign error code -4 (got ${foreign.code})")
        expect(foreign.message?.contains("refused boom") == true, "foreign message text (got ${foreign.message})")
        expect(store.count() == 0L, "boom was removed before the listener ran")
        expect(store.put("calm", payload, EntryKind.Persistent, null), "store usable after foreign error")
        expect(store.delete("calm"), "delete calm")
        expect(thrower.evictions == listOf("calm" to EvictionReason.Deleted), "listener still attached after throwing")
        store.clearEvictionListener()

        // --- Object graph: share / fork / larger / describe / openMany -----

        expect(store.put("one", payload, EntryKind.Persistent, null), "put one")
        val shared = store.share()
        expect(shared !== store, "share() returns a distinct wrapper")
        expect(shared.handle == store.handle, "share() wraps the same native object")
        expect(shared.count() == 1L, "shared sees existing entries")
        expect(shared.put("two", payload, EntryKind.Persistent, null), "put through shared")
        expect(store.count() == 2L, "write through shared is visible through the original")
        expect(store.get("two")!!.key == "two", "entry written through shared readable through original")
        shared.close()
        shared.close()
        expect(thrownBy { shared.count() } is IllegalStateException, "closed shared wrapper rejects use")
        expect(store.count() == 2L, "original still alive after closing the shared wrapper")

        val forked = store.fork()
        expect(forked.handle != store.handle, "fork() is a new object")
        expect(forked.count() == 2L, "fork copies live entries")
        expect(forked.put("three", payload, EntryKind.Persistent, null), "put into fork")
        expect(forked.count() == 3L && store.count() == 2L, "fork is independent")
        expect(forked.listKeys(null).asSequence().toList() == listOf("one", "three", "two"), "fork keys")

        // `Store?` both ways.
        Store.open("/tmp/empty").use { emptyStore ->
            expect(emptyStore.larger(null) == null, "empty.larger(null) is null")
            val own = store.larger(null)
            expect(own != null && own.handle == store.handle, "store.larger(null) is the store itself")
            own!!.close()
            val bigger = store.larger(forked)
            expect(bigger != null && bigger.handle == forked.handle, "store.larger(fork) is the fork")
            bigger!!.close()
            val self = forked.larger(emptyStore)
            expect(self != null && self.handle == forked.handle, "fork.larger(empty) is the fork")
            self!!.close()
            expect(store.larger(emptyStore)!!.use { it.handle } == store.handle, "store.larger(empty) is the store")
        }

        // Objects inside a record, with the optional absent and present.
        val info = store.describe("primary", null)
        expect(info.label == "primary", "describe label")
        expect(info.count == 2L, "describe count (got ${info.count})")
        expect(info.mirror == null, "describe mirror absent")
        expect(info.store.handle == store.handle, "describe().store is the described object")
        expect(info.store.count() == 2L, "describe().store is usable")
        val mirrored = store.describe("mirrored", forked)
        val mirror = mirrored.mirror
        expect(mirror != null && mirror.handle == forked.handle, "describe mirror present")
        expect(mirror!!.count() == 3L, "mirror usable")

        // A list of objects as a return, and the typed error from the static.
        val many = Store.openMany(listOf("/a", "/b", "/c"))
        expect(many.size == 3, "openMany returns 3 stores (got ${many.size})")
        expect(many.map { it.handle }.toSet().size == 3, "openMany stores are distinct")
        expect(many[0].put("m", payload, EntryKind.Persistent, null), "put into openMany[0]")
        expect(many[1].put("n", payload, EntryKind.Persistent, null), "put into openMany[1]")
        expect(many.map { it.count() } == listOf(1L, 1L, 0L), "openMany counts")
        val manyErr = thrownBy { Store.openMany(listOf("/ok", "")) }
        expect(manyErr is KvException.IoError, "openMany with an empty path throws IoError (got $manyErr)")

        // A list of objects and an object inside a record as parameters. The
        // encoder mints one reference per object, so the wrappers stay valid.
        expect(Store.totalCount(listOf(), null) == 0L, "totalCount of nothing")
        expect(Store.totalCount(many, null) == 2L, "totalCount over openMany (got ${Store.totalCount(many, null)})")
        expect(Store.totalCount(listOf(store, forked), null) == 5L, "totalCount store + fork")
        expect(Store.totalCount(listOf(store, forked), info) == 7L, "totalCount with extra record")
        expect(Store.totalCount(listOf(forked, forked), mirrored) == 8L, "totalCount repeats and mirror record")
        expect(store.count() == 2L && forked.count() == 3L, "stores still alive after being encoded")
        expect(many.all { it.handle != 0L }, "openMany wrappers still alive after being encoded")

        // Release every wrapper we minted; the originals keep working.
        info.store.close()
        mirrored.store.close()
        mirror.close()
        many.forEach { it.close() }
        expect(thrownBy { Store.totalCount(many, null) } is IllegalStateException, "encoding a closed store throws")
        expect(store.count() == 2L, "store alive after releasing record and list references")
        expect(forked.count() == 3L, "fork alive after releasing record references")
        forked.close()

        // clear() drops everything that remains.
        expect(store.put("last", payload, EntryKind.Persistent, null), "put last")
        store.clear()
        expect(store.count() == 0L, "store empty after clear")
    }

    // Close semantics: `use` closed it; a second close is safe and any use
    // afterwards throws.
    val closed = Store.open("/tmp/closed")
    closed.close()
    closed.close()
    expect(thrownBy { closed.count() } is IllegalStateException, "use after close throws")
    expect(thrownBy { closed.share() } is IllegalStateException, "share after close throws")
    expect(thrownBy { WeaveFFI.getStats(closed) } is IllegalStateException, "borrowing a closed store throws")

    println("kotlin/kvstore: OK")
}
