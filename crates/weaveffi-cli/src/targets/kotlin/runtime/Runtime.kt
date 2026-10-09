package {{PACKAGE}}

import java.lang.ref.PhantomReference
import java.lang.ref.ReferenceQueue
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
 * The native iterator is released when the sequence is exhausted, when
 * [close] is called, or once this object becomes unreachable, whichever
 * comes first.
 */
class NativeIterator<T> internal constructor(
    address: Long,
    release: (Long) -> Unit,
    private val step: (Long) -> Any?,
) : Iterator<T>, AutoCloseable {
    private val handle = NativeCleaner.register(this, NativeHandle(address, release))
    private var lookahead: Any? = EMPTY

    override fun hasNext(): Boolean {
        if (lookahead !== EMPTY) return true
        if (handle.isClosed) return false
        val item = handle.borrow(step)
        if (item === DONE) {
            close()
            return false
        }
        lookahead = item
        return true
    }

    override fun next(): T {
        if (!hasNext()) throw NoSuchElementException()
        val item = lookahead
        lookahead = EMPTY
        @Suppress("UNCHECKED_CAST")
        return item as T
    }

    /** Releases the native iterator; safe to call more than once. */
    override fun close() = handle.close()

    internal companion object {
        /** What a step function returns once the native sequence is exhausted. */
        val DONE = Any()
        private val EMPTY = Any()
    }
}

/** Loads the producer library and then the JNI shim, exactly once. */
internal object NativeLibrary {
    private const val LIBRARY = "{{LIBRARY}}"
    private const val JNI_LIBRARY = "{{JNI_LIBRARY}}"

    init {
        val explicit = System.getenv("{{LIBRARY_ENV}}")
        if (!explicit.isNullOrEmpty()) {
            System.load(explicit)
        } else if (!loadBundled(LIBRARY)) {
            // The shim's own link dependency usually finds the producer when
            // it isn't on java.library.path, so a miss here is only fatal if
            // loading the shim fails too.
            try {
                System.loadLibrary(LIBRARY)
            } catch (ignored: UnsatisfiedLinkError) {
            }
        }
        if (!loadBundled(JNI_LIBRARY)) System.loadLibrary(JNI_LIBRARY)
    }

    fun ensureLoaded() {}

    /**
     * Loads `base` from the `natives/<os>-<arch>/` classpath resources a
     * packaged desktop build bundles; false when it isn't bundled.
     */
    private fun loadBundled(base: String): Boolean {
        val os = System.getProperty("os.name").lowercase()
        val arch = System.getProperty("os.arch").lowercase()
        val osId = when {
            os.contains("mac") || os.contains("darwin") -> "darwin"
            os.contains("win") -> "windows"
            else -> "linux"
        }
        val archId = if (arch == "aarch64" || arch == "arm64") "arm64" else "x64"
        val file = when (osId) {
            "darwin" -> "lib$base.dylib"
            "windows" -> "$base.dll"
            else -> "lib$base.so"
        }
        val stream = NativeLibrary::class.java.getResourceAsStream("/natives/$osId-$archId/$file") ?: return false
        val dir = java.nio.file.Files.createTempDirectory("$base-natives")
        val target = dir.resolve(file)
        stream.use { java.nio.file.Files.copy(it, target) }
        target.toFile().deleteOnExit()
        dir.toFile().deleteOnExit()
        System.load(target.toAbsolutePath().toString())
        return true
    }
}

/**
 * One strong reference to a native object, released exactly once: by
 * [close], or by [NativeCleaner] once the owning wrapper is unreachable.
 * Every call borrows the address for its duration, and a close that races an
 * in-flight call defers the release until the last borrow ends, so the object
 * is never freed while native code is using it.
 */
internal class NativeHandle(val address: Long, private val release: (Long) -> Unit) {
    // Bit 0 is the closed flag; the remaining bits count in-flight borrows.
    private val state = AtomicLong(0L)

    val isClosed: Boolean get() = state.get() and 1L != 0L

    fun acquire(): Long {
        while (true) {
            val s = state.get()
            check(s and 1L == 0L) { "native object used after close()" }
            if (state.compareAndSet(s, s + 2L)) return address
        }
    }

    fun unacquire() {
        if (state.addAndGet(-2L) == 1L) release(address)
    }

    fun close() {
        while (true) {
            val s = state.get()
            if (s and 1L != 0L) return
            if (state.compareAndSet(s, s or 1L)) {
                if (s == 0L) release(address)
                return
            }
        }
    }
}

/** Runs [block] with the native address borrowed for its duration. */
internal inline fun <R> NativeHandle.borrow(block: (Long) -> R): R {
    val address = acquire()
    try {
        return block(address)
    } finally {
        unacquire()
    }
}

/** [borrow] for an optional object: a missing object lends address 0. */
internal inline fun <R> NativeHandle?.borrowOrNull(block: (Long) -> R): R =
    if (this == null) block(0L) else borrow(block)

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
            } catch (_: Throwable) {
            }
        }
    }
}

/** UTF-8 bytes of [value], for a string crossing into native code. */
internal fun encodeUtf8(value: String): ByteArray = value.toByteArray(Charsets.UTF_8)

/** The string a native UTF-8 byte sequence spells. */
internal fun decodeUtf8(bytes: ByteArray): String = String(bytes, Charsets.UTF_8)
