// Conformance consumer: kvstore sample, Kotlin (JVM via JNI) target.
//
// Drives the feature-complete producer through the generated bindings:
//
//   * the load-time checks (ABI revision, both modules' contract tables),
//     which loading the bindings performs;
//   * `Store`: fallible and infallible constructors (`Store.open`, `Store()`),
//     methods, statics, the deprecated `size()`, the `Entry` and `StoreInfo`
//     data classes, the `EntryKind` enum class, maps and optionals, unsigned
//     counts as `UInt`/`ULong`, and the logical clock;
//   * `KvException` subclasses with their payload fields (`KeyNotFound`,
//     `Expired`, `StoreFull`, `Rejected`), and runtime failures (-4) as the
//     root `FfiException`;
//   * lazy `NativeIterator`s of strings (throwing at launch), records, and
//     objects, including one abandoned part-way;
//   * three callback interfaces implemented in Kotlin: a `Listener`
//     (retained, filtered by `accepts`, told about every `Change`, detached
//     when it throws, and notified from a producer thread during
//     compaction), a `Policy` (a record return, a throwing method whose
//     `KvException.Rejected` reaches the `put` caller with its payload, an
//     object parameter and object return), and a `Loader` passed as an
//     optional callback (string, bytes, and optional-object returns; typed
//     errors decoded by the producer or passed through);
//   * `Store` objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * suspend functions: an async free function returning an object, a
//     cancellable method cancelled mid-pause (completing at once while its
//     background work stops cooperatively, shown by `activeJobs`), an async
//     list launched concurrently, and an async function in the nested
//     `kv.stats` module (`Kv.StatsModule`);
//   * the sibling `report` root (the shared `Entry` record and its own error
//     domain).
//
// Releases of consumer callbacks are observed through the producer's
// callback counter (`debug_live(1)`). Ends by asserting the producer's leak
// counters are zero. Compiled with `-Xfriend-paths`, so the bindings'
// `internal` members (the bridge and each wrapper's handle) are reachable.
@file:JvmName("Main")

import java.util.Collections
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kvstore.Change
import kvstore.Entry
import kvstore.EntryKind
import kvstore.FfiException
import kvstore.JniBridge
import kvstore.Kv
import kvstore.KvException
import kvstore.Listener
import kvstore.Loader
import kvstore.Policy
import kvstore.Report
import kvstore.ReportException
import kvstore.Stats
import kvstore.Store
import kvstore.StoreInfo

val mainThread: Thread = Thread.currentThread()

/** Live consumer callbacks the producer holds. */
fun liveCallbacks(): Long = JniBridge.debug_live(1)

/** The native object a wrapper refers to (identity, not value). */
fun Store.address(): Long = handle.address

fun bytes(s: String) = s.toByteArray(Charsets.UTF_8)

fun text(b: ByteArray) = String(b, Charsets.UTF_8)

fun Store.putText(key: String, value: String, kind: EntryKind = EntryKind.Persistent, ttl: Long? = null): Entry =
    put(key, bytes(value), kind, ttl)

inline fun <reified E : Throwable> expectThrows(what: String, block: () -> Unit): E {
    val e = thrownBy(block)
    expect(e is E, "$what raises ${E::class.simpleName} (got $e)")
    return e as E
}

fun expectKeyNotFound(key: String, block: () -> Unit) {
    val e = expectThrows<KvException.KeyNotFound>("key $key", block)
    expect(e.code == 1001 && e.key == key, "KeyNotFound payload key $key (got ${e.key})")
}

// ── listener (consumer-implemented, retained) ─────────────────────────────

class RecordingListener(private val skip: String? = null, private val failOn: String? = null) : Listener {
    val changes: MutableList<Change> = Collections.synchronizedList(mutableListOf())
    val offMainThread = AtomicInteger()

    override fun accepts(key: String): Boolean {
        if (key == failOn) throw IllegalStateException("listener refused")
        return key != skip
    }

    override fun onChange(change: Change) {
        if (Thread.currentThread() !== mainThread) offMainThread.incrementAndGet()
        changes.add(change)
    }

    fun puts() = changes.filterIsInstance<Change.Put>()

    fun removed(expired: Boolean) = changes.filterIsInstance<Change.Removed>().filter { it.expired == expired }
}

// ── policy (consumer-implemented, rich returns, throws) ───────────────────

/** Routes `b/` keys to [other], a reference this policy owns. */
class TestPolicy(private val other: Store) : Policy {
    val admitted = AtomicInteger()

    override fun admit(entry: Entry): Entry {
        admitted.incrementAndGet()
        expect(entry.version == 0u, "the store assigns the version after admission")
        return when {
            entry.key.startsWith("secret") ->
                throw KvException.Rejected(entry.key, "no secrets", "secrets are not stored")
            entry.key.startsWith("boom") -> throw IllegalStateException("policy exploded")
            // Tag it and store it encrypted (and try to rename it, which the
            // store ignores).
            else -> entry.copy(key = "renamed", kind = EntryKind.Encrypted, tags = listOf("admitted"))
        }
    }

    override fun route(key: String, home: Store): Store = if (key.startsWith("b/")) other else home
}

// ── loader (consumer-implemented, passed as an optional parameter) ────────

class TestLoader(private val backup: Store? = null) : Loader {
    override fun name() = "kotlin-loader"

    override fun fallback(key: String): Store? = if (key == "fb") backup else null

    override fun load(key: String): ByteArray = when (key) {
        "missing" -> throw KvException.KeyNotFound("missing", "not in the loader")
        "elsewhere" -> throw KvException.KeyNotFound("other", "not in the loader")
        "broken" -> throw IllegalStateException("loader is broken")
        else -> bytes("loaded:$key")
    }
}

// ── sections ──────────────────────────────────────────────────────────────

fun constructors() {
    val e = expectThrows<KvException.InvalidPath>("Store.open(\"\")") { Store.open("") }
    expect(e.code == 1004 && e.message == "invalid path", "InvalidPath code and message")

    Store().use { s ->
        expect(s.path() == "memory", "Store() path")
        expect(s.capacity() == Store.defaultCapacity(), "capacity == defaultCapacity")
        expect(Store.defaultCapacity() == 1_000_000u, "defaultCapacity == 1000000")
    }

    runBlocking {
        Kv.openStore("/async").use { s -> expect(s.path() == "/async", "openStore path") }
        val err = thrownBy { Kv.openStore("") }
        expect(err is KvException.InvalidPath, "openStore(\"\") rejects with InvalidPath (got $err)")
    }
}

fun basics() = Store.open("/basics").use { s ->
    // put returns the stored entry; the version counts puts of the key.
    val e1 = s.putText("alpha", "one", EntryKind.Persistent)
    expect(e1.key == "alpha" && text(e1.value) == "one" && e1.kind == EntryKind.Persistent, "put alpha")
    expect(e1.version == 1u && e1.expires_at == null && e1.tags.isEmpty() && e1.metadata.isEmpty(), "put alpha fields")
    val e2 = s.putText("alpha", "two", EntryKind.Volatile)
    expect(e2.version == 2u && e2.kind == EntryKind.Volatile, "put alpha again")

    expect(text(s.get("alpha").value) == "two", "get alpha")
    val missing = expectThrows<KvException.KeyNotFound>("get(nope)") { s.get("nope") }
    expect(missing.key == "nope" && missing.message == "key not found: nope", "KeyNotFound(nope)")
    expect(s.find("alpha")?.version == 2u, "find alpha")
    expect(s.find("nope") == null, "find nope")

    // TTLs follow the logical clock; an expired get reports when.
    expect(s.putText("ttl", "x", EntryKind.Volatile, 10).expires_at == 10L, "ttl expires_at")
    expect(s.now() == 0L, "clock starts at 0")
    expect(s.tick(9) == 9L && s.count() == 2u, "tick 9")
    expect(s.tick(1) == 10L && s.count() == 1u, "tick 10")
    val expired = expectThrows<KvException.Expired>("get(ttl)") { s.get("ttl") }
    expect(expired.key == "ttl" && expired.expired_at == 10L, "Expired payload")
    expectKeyNotFound("ttl") { s.get("ttl") } // the expired read removed it

    // Capacity: a new key past it is StoreFull { capacity }.
    s.setCapacity(1u)
    expect(s.capacity() == 1u, "setCapacity")
    s.putText("alpha", "three") // replacing is fine
    val full = expectThrows<KvException.StoreFull>("put beta at capacity") { s.putText("beta", "b") }
    expect(full.capacity == 1u, "StoreFull capacity")
    s.setCapacity(100u)

    // delete, clear, and the deprecated size().
    s.putText("beta", "b")
    expect(s.delete("beta") && !s.delete("beta"), "delete twice")
    @Suppress("DEPRECATION")
    val size = s.size()
    expect(size == s.count() && size == 1u, "deprecated size() == count()")
    expect(s.clear() == 1u && s.count() == 0u, "clear")
}

fun iterators() = Store.open("/iter").use { s ->
    s.putText("user.bob", "b")
    s.putText("user.alice", "a")
    s.putText("sys.x", "xx")

    expect(s.keys(null).asSequence().toList() == listOf("sys.x", "user.alice", "user.bob"), "keys in order")
    expect(JniBridge.debug_live(2) == 0L, "an exhausted iterator is released")
    expectKeyNotFound("zzz") { s.keys("zzz") }

    // Abandoning an iterator part-way releases it.
    s.keys("user.").use { keys ->
        expect(keys.next() == "user.alice", "first user key")
        expect(JniBridge.debug_live(2) == 1L, "the iterator is live")
    }
    expect(JniBridge.debug_live(2) == 0L, "an abandoned iterator is released")

    val sys = s.entries("sys.").asSequence().toList()
    expect(sys.size == 1 && sys[0].key == "sys.x" && text(sys[0].value) == "xx", "entries(sys.)")

    // partition: objects, created as they're pulled.
    val prefixes = listOf("user.", "sys.", "none.")
    var i = 0
    for (part in s.partition(prefixes)) {
        part.use {
            expect(it.address() != s.address(), "partition yields new stores")
            expect(it.count() == listOf(2u, 1u, 0u)[i] && it.path() == prefixes[i], "partition $i")
        }
        i++
    }
    expect(i == 3, "three partitions")
}

fun listeners() {
    val base = liveCallbacks()
    val s = Store.open("/listen")
    val l = RecordingListener(skip = "quiet")
    val id = s.subscribe(l)
    expect(id > 0u && s.listenerCount() == 1u && liveCallbacks() == base + 1, "subscribe")

    s.putText("a", "1")
    var put = l.puts().last()
    expect(l.puts().size == 1 && put.entry.version == 1u && !put.replaced, "Put v1")
    s.putText("a", "2")
    put = l.puts().last()
    expect(l.puts().size == 2 && put.entry.version == 2u && put.replaced && put.entry.key == "a", "Put v2")
    s.putText("quiet", "x") // accepts() said no
    expect(l.puts().size == 2, "the filter skipped quiet")
    expect(s.delete("a"), "delete a")
    expect(l.changes.last() == Change.Removed("a", false), "Removed(a)")

    // An expired read removes the entry and says so.
    s.putText("short", "x", EntryKind.Volatile, 1)
    s.tick(1)
    expectThrows<KvException.Expired>("get(short)") { s.get("short") }
    expect(l.changes.last() == Change.Removed("short", true), "Removed(short, expired)")

    expect(s.clear() == 1u, "clear leaves quiet's count") // "quiet" was left
    expect(l.changes.last() == Change.Cleared(1u), "Cleared(1)")
    expect(l.offMainThread.get() == 0, "synchronous calls notify on the calling thread")

    // Unsubscribing releases the listener once.
    expect(s.unsubscribe(id) && liveCallbacks() == base, "unsubscribe releases the listener")
    expect(!s.unsubscribe(id) && s.listenerCount() == 0u, "unsubscribe twice")

    // A listener that fails is detached (and released); the put succeeds.
    val failing = RecordingListener(failOn = "boom")
    s.subscribe(failing)
    s.putText("fine", "1")
    expect(failing.puts().size == 1, "the failing listener saw fine")
    s.putText("boom", "1")
    expect(s.count() == 2u && s.listenerCount() == 0u, "a failing listener is detached")
    expect(liveCallbacks() == base, "the failing listener was released")

    // Closing the store releases the listeners it still holds.
    s.subscribe(RecordingListener())
    s.subscribe(RecordingListener())
    expect(s.listenerCount() == 2u && liveCallbacks() == base + 2, "two listeners")
    s.close()
    expect(liveCallbacks() == base, "closing the store released its listeners")
}

fun policies() {
    val base = liveCallbacks()
    val s = Store.open("/policy")
    val other = Store.open("/other")
    val p = TestPolicy(other.share())
    s.setPolicy(p)
    expect(s.hasPolicy() && liveCallbacks() == base + 1, "setPolicy")

    // admit's record return is what's stored (its key and version aside).
    val a = s.putText("a", "1", EntryKind.Volatile)
    expect(a.key == "a" && a.version == 1u && a.kind == EntryKind.Encrypted, "admitted entry")
    expect(a.tags == listOf("admitted"), "admit's rewrite")

    // route: the object parameter and object return redirect a write.
    s.putText("b/x", "2", EntryKind.Volatile)
    expect(s.count() == 1u && other.count() == 1u, "route redirected b/x")

    // A typed error from the throwing callback reaches the caller with its
    // code, message, and payload.
    val rejected = expectThrows<KvException.Rejected>("put(secret)") { s.putText("secret", "3") }
    expect(rejected.code == 1005 && rejected.message == "secrets are not stored", "Rejected code and message")
    expect(rejected.key == "secret" && rejected.reason == "no secrets", "Rejected payload")

    // Anything else arrives as -4 with the consumer's message.
    val boom = expectThrows<FfiException>("put(boom)") { s.putText("boom", "4") }
    expect(boom !is KvException && boom.code == -4 && boom.message == "policy exploded", "-4 (got ${boom.code}: ${boom.message})")
    expect(s.count() == 1u && other.count() == 1u && p.admitted.get() == 4, "failed puts changed nothing")

    // Replacing the policy releases the old one; null removes it.
    s.setPolicy(TestPolicy(other.share()))
    expect(liveCallbacks() == base + 1, "replacing the policy released the old one")
    s.setPolicy(null)
    expect(!s.hasPolicy() && liveCallbacks() == base, "setPolicy(null) released it")
    s.putText("secret", "now allowed")
    expect(s.count() == 2u, "no policy, no veto")

    other.close()
    s.close()
}

fun loaders() = Store.open("/load").use { s ->
    val base = liveCallbacks()

    // No loader (a null optional callback): a miss is none.
    expect(s.getOrLoad("k", null) == null, "no loader")

    // load's bytes are stored, tagged with the loader's name.
    val loaded = s.getOrLoad("k", TestLoader())
    expect(loaded != null && text(loaded.value) == "loaded:k", "loaded value")
    expect(loaded!!.kind == EntryKind.Volatile && loaded.metadata == mapOf("source" to "kotlin-loader"), "loaded entry")
    expect(liveCallbacks() == base, "a loader is released after the call")
    expect(s.count() == 1u, "the loaded entry is stored")
    // A hit doesn't consult the loader.
    expect(s.getOrLoad("k", TestLoader())?.version == 1u, "a hit")

    // The fallback store (an optional object return) is consulted first.
    Store.open("/backup").use { backup ->
        backup.putText("fb", "from backup")
        val fb = s.getOrLoad("fb", TestLoader(backup.share()))
        expect(fb != null && text(fb.value) == "from backup" && fb.kind == EntryKind.Persistent, "fallback entry")
    }

    // KeyNotFound for this key: the producer decoded the payload and
    // answers none.
    expect(s.getOrLoad("missing", TestLoader()) == null, "KeyNotFound for the same key is none")
    // KeyNotFound for another key: passed through, payload intact.
    val other = expectThrows<KvException.KeyNotFound>("getOrLoad(elsewhere)") { s.getOrLoad("elsewhere", TestLoader()) }
    expect(other.key == "other" && other.message == "not in the loader", "KeyNotFound(other) passed through")
    // Any other failure is -4.
    val broken = expectThrows<FfiException>("getOrLoad(broken)") { s.getOrLoad("broken", TestLoader()) }
    expect(broken.code == -4 && broken.message == "loader is broken", "-4 (got ${broken.code}: ${broken.message})")
    expect(liveCallbacks() == base, "every loader was released")
}

fun asyncCalls() = runBlocking {
    val s = Store.open("/async-calls")
    val l = RecordingListener()
    s.subscribe(l)
    s.putText("old1", "x", EntryKind.Volatile, 1)
    s.putText("old2", "x", EntryKind.Volatile, 1)
    s.putText("keep", "x")
    s.tick(5)

    // compact runs on a producer thread and notifies listeners there.
    expect(s.compact(0u) == 2u, "compact(0) removed 2")
    expect(l.removed(expired = true).size == 2, "the listener saw both expirations")
    expect(l.offMainThread.get() == 2, "notified from a producer thread")
    expect(s.count() == 1u, "one entry left")
    expect(s.compact(5u) == 0u, "compact(5) removed nothing")

    // Cancel mid-pause: the call completes at once with a cancellation, and
    // the background pause notices the token and stops.
    val job = async { s.compact(60_000u) }
    delay(20)
    expect(!job.isCompleted, "compact(60000) is pausing")
    expect(Store.activeJobs() >= 1u, "the pause is running")
    val started = System.nanoTime()
    job.cancel()
    val cancelled = thrownBy { job.await() }
    expect(cancelled is CancellationException, "a cancelled compact raises CancellationException (got $cancelled)")
    expect(System.nanoTime() - started < 2_000_000_000L, "cancellation is prompt")
    var stopped = false
    for (i in 0 until 2000) {
        if (Store.activeJobs() == 0u) {
            stopped = true
            break
        }
        delay(1)
    }
    expect(stopped, "the cancelled pause stopped cooperatively")

    // getMany: an async list of optional records, launched concurrently.
    val results = (0 until 32).map {
        async(Dispatchers.Default) { s.getMany(listOf("keep", "gone", "keep")) }
    }.awaitAll()
    for (got in results) {
        expect(got.size == 3 && got[0]?.key == "keep" && got[1] == null && got[2]?.key == "keep", "getMany")
    }

    // The nested module's async function: objects in a list in, a record out.
    Store.open("/other").use { other ->
        other.putText("a", "123")
        val st = Kv.StatsModule.summarizeAll(listOf(s, other))
        expect(st == Stats(2u, 4uL, mapOf(EntryKind.Persistent to 2u)), "summarizeAll (got $st)")
    }
    s.close()
}

fun objectGraph() {
    val s0 = Store.open("/graph")
    s0.putText("k", "v")

    // share(): the same object; the original can go.
    val s = s0.share()
    expect(s.address() == s0.address(), "share() is the same object")
    s0.close()
    expect(s.count() == 1u, "alive through the shared reference")

    // fork(): a distinct object with a copy of the entries.
    val fork = s.fork()
    expect(fork.address() != s.address() && fork.count() == 1u && fork.path() == "/graph", "fork")
    fork.putText("k2", "v")
    expect(fork.count() == 2u && s.count() == 1u, "fork is independent")

    // larger(): `Store?` in and out.
    val empty = Store.open("/empty")
    expect(empty.larger(null) == null, "larger(null) on an empty store")
    empty.larger(fork)!!.use { expect(it.address() == fork.address(), "larger(fork) is fork") }
    s.larger(null)!!.use { expect(it.address() == s.address(), "larger(null) is self") }

    // describe(): a record whose fields carry objects.
    val info = s.describe("main", fork)
    expect(info.label == "main" && info.count == 1u, "describe label and count")
    expect(info.store.address() == s.address() && info.mirror?.address() == fork.address(), "describe objects")
    expect(info.mirror?.count() == 2u, "the mirror is live")

    // openMany(): a list of objects; one bad path fails the whole call.
    val many = Store.openMany(listOf("/a", "/b"))
    expect(many.map { it.path() } == listOf("/a", "/b"), "openMany paths")
    expectThrows<KvException.InvalidPath>("openMany with an empty path") { Store.openMany(listOf("/a", "")) }

    // byLabel(): records with objects in, a map with object values out.
    val named = Store.byLabel(listOf(info, StoreInfo("first", many[0], null, 0u)))
    expect(named.keys == setOf("main", "first"), "byLabel keys")
    expect(named.getValue("main").address() == s.address(), "byLabel main")
    expect(named.getValue("first").address() == many[0].address(), "byLabel first")

    // totalCount(): a list, a map, and an optional record, all carrying
    // objects (each written as a fresh reference the producer adopts).
    many[0].putText("m", "1")
    val stores = listOf(many[0], many[1], fork)
    expect(Store.totalCount(stores, named, info) == 6u, "totalCount with extra")
    expect(Store.totalCount(stores, named, null) == 5u, "totalCount without extra")

    // Everything is still intact; release each reference once.
    expect(s.count() == 1u && fork.count() == 2u && many[0].count() == 1u, "still usable")
    named.values.forEach { it.close() }
    info.store.close()
    info.mirror?.close()
    many.forEach { it.close() }
    empty.close()
    fork.close()
    s.close()
}

fun statsAndReport() = Store.open("/stats").use { s ->
    s.putText("b", "12")
    s.putText("a", "1")
    s.putText("a", "123")
    s.putText("c", "x", EntryKind.Encrypted)

    // kv.stats: the parent's Store as a parameter, the parent's error domain.
    val st = Kv.StatsModule.summarize(s, null)
    expect(st == Stats(3u, 6uL, mapOf(EntryKind.Persistent to 2u, EntryKind.Encrypted to 1u)), "summarize (got $st)")
    expectKeyNotFound("q") { Kv.StatsModule.summarize(s, "q") }

    // report: the sibling root shares the Entry record.
    val lines = Report.renderReport(s.entries(null).asSequence().toList())
    expect(
        lines == listOf("a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent", "c: 1 bytes, Encrypted"),
        "renderReport (got $lines)",
    )
    val nothing = expectThrows<ReportException.NothingToReport>("renderReport([])") { Report.renderReport(emptyList()) }
    expect(nothing.code == 2001 && nothing.message == "nothing to report", "NothingToReport")
}

fun main() {
    expect(JniBridge.debug_live(-1) == 1L, "the sample counts live allocations")

    constructors()
    basics()
    iterators()
    listeners()
    policies()
    loaders()
    asyncCalls()
    objectGraph()
    statsAndReport()

    expectNoLeaks(JniBridge::debug_live)
    println("kotlin/kvstore: OK")
}
