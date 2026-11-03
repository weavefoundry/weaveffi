package {{PACKAGE}}

import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.Structure
import java.lang.ref.PhantomReference
import java.lang.ref.ReferenceQueue
import java.lang.reflect.Proxy
import java.util.Collections
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * A failure a throwing call reports. Each error domain subclasses it with
 * one class per declared (positive) code; a runtime failure keeps this class
 * with its negative [code]: -1 a generic producer error, -2 a producer
 * panic, -3 a marshalling failure, or -4 a callback implementation that
 * failed.
 */
open class FfiException(val code: Int, message: String) : Exception(message) {
    /**
     * The fields of a typed error as the value buffer a callback reports
     * with its code, or `null` when the code declares none.
     */
    internal open fun encodePayload(): ByteArray? = null
}

/**
 * A call that can't fail failed anyway: a bug in the native library rather
 * than an error to handle. [code] is the runtime code (-2 a producer panic,
 * -3 a marshalling failure, -4 a callback failure the call couldn't report,
 * and so on); the message names it and carries the producer's message.
 */
class NativeBugException(val code: Int, message: String) :
    IllegalStateException("native call failed with code $code: $message")

/**
 * A lazy sequence streamed from the native library, one element per step.
 * It can be iterated once. The native iterator is released when the
 * sequence is exhausted, when [close] is called, or once this object becomes
 * unreachable, whichever comes first. Not thread-safe: iterate it from one
 * thread at a time.
 */
class NativeSequence<T> internal constructor(
    iterator: Pointer,
    release: (Pointer) -> Unit,
    private val advance: (Pointer) -> Any?,
) : Sequence<T>, AutoCloseable {
    private val handle = NativeCleaner.register(this, NativeHandle(iterator, release))
    private var started = false

    override fun iterator(): Iterator<T> {
        check(!started) { "a NativeSequence can be iterated only once" }
        started = true
        return Steps()
    }

    /** Releases the native iterator; safe to call more than once. */
    override fun close() = handle.close()

    private inner class Steps : Iterator<T> {
        private var ready = false
        private var lookahead: Any? = null

        override fun hasNext(): Boolean {
            if (ready) return true
            if (handle.isClosed) return false
            val item = handle.borrow { advance(it) }
            if (item === DONE) {
                close()
                return false
            }
            lookahead = item
            ready = true
            return true
        }

        override fun next(): T {
            if (!hasNext()) throw NoSuchElementException()
            @Suppress("UNCHECKED_CAST")
            val item = lookahead as T
            lookahead = null
            ready = false
            return item
        }
    }

    internal companion object {
        /** What a step returns when the native iterator is exhausted. */
        val DONE = Any()
    }
}

/**
 * Loads the native library, exactly once, through JNA, and checks that it
 * implements the C ABI revision and every declaration these bindings were
 * generated against.
 *
 * The first use of the bindings loads it implicitly. Call [load] first to
 * handle a missing or mismatched library gracefully: it throws the same
 * [UnsatisfiedLinkError] on every call after a failure, while a binding
 * used after a failed load throws that error the first time and
 * [NoClassDefFoundError] after (both are [LinkageError]s).
 *
 * The library loads from the path in the `{{LIBRARY_ENV}}` environment
 * variable when it's set, else wherever JNA finds `{{LIBRARY}}`: the
 * `jna.library.path` system property, the system's library search path,
 * an Android app's native libraries, or the `<os>-<arch>/` classpath
 * resources of a packaged desktop build (`darwin-aarch64/`,
 * `linux-x86-64/`, `win32-x86-64/`, ...).
 */
object NativeLibrary {
    private const val LIBRARY = "{{LIBRARY}}"
    private const val ABI_VERSION = {{ABI_VERSION}}

    private val loaded: Any = try {
        open()
    } catch (t: Throwable) {
        t
    }

    /**
     * Loads and checks the native library if that hasn't happened yet.
     *
     * @throws UnsatisfiedLinkError when the library can't be loaded,
     *   implements a different C ABI revision, or lacks or changed a
     *   declaration these bindings use; the message says which.
     */
    @JvmStatic
    fun load() {
        api()
    }

    /** The checked library, or the load failure as an [UnsatisfiedLinkError]. */
    internal fun api(): CLibrary {
        val result = loaded
        if (result is CLibrary) return result
        val cause = result as Throwable
        val error = UnsatisfiedLinkError(cause.message ?: cause.toString())
        error.initCause(cause)
        throw error
    }

    private fun open(): CLibrary {
        val explicit = System.getenv("{{LIBRARY_ENV}}")
        val name = if (explicit.isNullOrEmpty()) LIBRARY else explicit
        val options = mapOf(Library.OPTION_CLASSLOADER to NativeLibrary::class.java.classLoader)
        val api = try {
            Native.load(name, CLibrary::class.java, options)
        } catch (e: UnsatisfiedLinkError) {
            throw UnsatisfiedLinkError("can't load the {{NAME}} library ($name): ${e.message}")
        }
        val library = (Proxy.getInvocationHandler(api) as Library.Handler).nativeLibrary
        val revision = function(library, "{{PREFIX}}_abi_version")
            ?: throw UnsatisfiedLinkError("$name isn't a {{NAME}} library: it doesn't export {{PREFIX}}_abi_version")
        val found = revision.invokeInt(emptyArray())
        if (found != ABI_VERSION) {
            throw UnsatisfiedLinkError(
                "$name implements C ABI revision $found, but these bindings need revision $ABI_VERSION",
            )
        }
        for (row in CONTRACT) {
            val check = function(library, row.symbol)
                ?: throw UnsatisfiedLinkError("`${row.path}` is missing from the {{NAME}} library ($name)")
            if (check.invokeLong(emptyArray()) != row.hash) {
                throw UnsatisfiedLinkError("`${row.path}` changed since these bindings were generated ($name)")
            }
        }
        return api
    }

    private fun function(library: com.sun.jna.NativeLibrary, symbol: String): com.sun.jna.Function? =
        try {
            library.getFunction(symbol)
        } catch (e: UnsatisfiedLinkError) {
            null
        }
}

/** One declaration these bindings check at load: its contract function and hash. */
internal class ContractRow(val symbol: String, val hash: Long, val path: String)

/** The loaded library's C functions; the first use loads and checks it. */
internal object Lib {
    @JvmField
    val c: CLibrary = NativeLibrary.api()
}

/**
 * One strong reference to a native object, released exactly once: by
 * [close], or by [NativeCleaner] once the owning wrapper is unreachable.
 * Every call borrows the pointer for its duration, and a close that races an
 * in-flight call defers the release until the last borrow ends, so the object
 * is never freed while native code is using it.
 */
internal class NativeHandle(val pointer: Pointer, private val release: (Pointer) -> Unit) {
    // Bit 0 is the closed flag; the remaining bits count in-flight borrows.
    private val state = AtomicLong(0L)

    val isClosed: Boolean get() = state.get() and 1L != 0L

    fun acquire(): Pointer {
        while (true) {
            val s = state.get()
            check(s and 1L == 0L) { "native object used after close()" }
            if (state.compareAndSet(s, s + 2L)) return pointer
        }
    }

    fun unacquire() {
        if (state.addAndGet(-2L) == 1L) release(pointer)
    }

    fun close() {
        while (true) {
            val s = state.get()
            if (s and 1L != 0L) return
            if (state.compareAndSet(s, s or 1L)) {
                if (s == 0L) release(pointer)
                return
            }
        }
    }
}

/** Runs [block] with the native pointer borrowed for its duration. */
internal inline fun <R> NativeHandle.borrow(block: (Pointer) -> R): R {
    val pointer = acquire()
    try {
        return block(pointer)
    } finally {
        unacquire()
    }
}

/** [borrow] for an optional object: a missing object lends `null`. */
internal inline fun <R> NativeHandle?.borrowOrNull(block: (Pointer?) -> R): R =
    if (this == null) block(null) else borrow(block)

/**
 * Releases the native reference of every wrapper that becomes unreachable
 * without being closed. A daemon thread drains a phantom-reference queue, so
 * this works on every Android API level and every JVM.
 */
internal object NativeCleaner {
    private val queue = ReferenceQueue<Any>()
    private val pending: MutableSet<Entry> = Collections.newSetFromMap(ConcurrentHashMap())

    private class Entry(owner: Any, val handle: NativeHandle, queue: ReferenceQueue<Any>) :
        PhantomReference<Any>(owner, queue)

    init {
        val thread = Thread({ drain() }, "{{PACKAGE}}-cleaner")
        thread.isDaemon = true
        thread.start()
    }

    /** Releases [handle] once [owner] is unreachable; returns [handle]. */
    fun register(owner: Any, handle: NativeHandle): NativeHandle {
        pending.add(Entry(owner, handle, queue))
        return handle
    }

    private fun drain() {
        while (true) {
            val entry = queue.remove() as Entry
            pending.remove(entry)
            try {
                entry.handle.close()
            } catch (ignored: Throwable) {
            }
        }
    }
}

/** A non-null object pointer the library returned, or a bug report. */
internal fun nonNull(pointer: Pointer?): Pointer =
    pointer ?: throw NativeBugException(-3, "the library returned a null object")

/**
 * Makes a synchronous call with a fresh error slot and throws the failure
 * it reports, mapped through [domain] (see [nativeError]).
 */
internal inline fun <R> callNative(domain: Int, call: (NativeError) -> R): R {
    val err = NativeError()
    val result = call(err)
    if (err.code != 0) throw err.failure(domain)
    return result
}

/** The exception for the failure in this slot, which it then clears. */
internal fun NativeError.failure(domain: Int): Throwable {
    val code = code
    val message = message()
    val payload = payload()
    Lib.c.{{PREFIX}}_error_clear(this)
    return nativeError(domain, code, message, payload)
}

/**
 * The consumer implementations the library holds, by the context pointer
 * their vtable argument carries, until the library frees the context.
 */
internal object Callbacks {
    private val live = ConcurrentHashMap<Long, Any>()
    private val ids = AtomicLong()

    /** Every vtable's `free` entry: forgets the implementation. */
    @JvmField
    val FREE = FreeCallback { ctx ->
        if (ctx != null) live.remove(Pointer.nativeValue(ctx))
    }

    /** The argument passing [impl] through [vtable], which the library adopts. */
    fun arg(impl: Any, vtable: Structure): CallbackArg {
        val id = ids.incrementAndGet()
        live[id] = impl
        return CallbackArg(Pointer(id), vtable.pointer)
    }

    /** The implementation behind a vtable context. */
    @Suppress("UNCHECKED_CAST")
    fun <T> get(ctx: Pointer?): T {
        val impl = ctx?.let { live[Pointer.nativeValue(it)] }
            ?: throw IllegalStateException("callback called after the library released it")
        return impl as T
    }
}

/**
 * Reports a failed callback through its vtable entry's `out_err` ([err]):
 * [typed], an exception of the domain the method throws, with its code and
 * payload fields, and anything else with code [fallback] and its message.
 */
internal fun reportFailure(err: Pointer?, error: Throwable, typed: FfiException?, fallback: Int) {
    try {
        if (typed != null) {
            setError(err, typed.code, typed.message ?: "")
            val payload = typed.encodePayload()
            if (payload != null) {
                Lib.c.{{PREFIX}}_error_set_payload(err, payload, SizeT(payload.size.toLong()))
            }
        } else {
            setError(err, fallback, error.message ?: error.toString())
        }
    } catch (ignored: Throwable) {
        // Reporting never fails the producer; the zero return stands.
    }
}

private fun setError(err: Pointer?, code: Int, message: String) {
    val bytes = encodeUtf8(message)
    Lib.c.{{PREFIX}}_error_set(err, code, bytes, SizeT(bytes.size.toLong()))
}

/** UTF-8 bytes of [value], for a string crossing into native code. */
internal fun encodeUtf8(value: String): ByteArray = value.toByteArray(Charsets.UTF_8)

/** The string a native UTF-8 byte sequence spells. */
internal fun decodeUtf8(bytes: ByteArray): String = String(bytes, Charsets.UTF_8)

/** The bits of each element, for a `[u16]` crossing as a `ShortArray`. */
internal fun Collection<UShort>.toShortArrayBits(): ShortArray {
    val out = ShortArray(size)
    var i = 0
    for (v in this) out[i++] = v.toShort()
    return out
}

/** The bits of each element, for a `[u32]` crossing as an `IntArray`. */
internal fun Collection<UInt>.toIntArrayBits(): IntArray {
    val out = IntArray(size)
    var i = 0
    for (v in this) out[i++] = v.toInt()
    return out
}

/** The bits of each element, for a `[u64]` crossing as a `LongArray`. */
internal fun Collection<ULong>.toLongArrayBits(): LongArray {
    val out = LongArray(size)
    var i = 0
    for (v in this) out[i++] = v.toLong()
    return out
}

/** A `[u16]` that crossed as a `ShortArray`. */
internal fun ShortArray.toUShortList(): List<UShort> = List(size) { this[it].toUShort() }

/** A `[u32]` that crossed as an `IntArray`. */
internal fun IntArray.toUIntList(): List<UInt> = List(size) { this[it].toUInt() }

/** A `[u64]` that crossed as a `LongArray`. */
internal fun LongArray.toULongList(): List<ULong> = List(size) { this[it].toULong() }
