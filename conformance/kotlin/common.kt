// Helpers shared by the Kotlin conformance consumers; each lane compiles
// this file together with its consumer.

import java.lang.ref.WeakReference
import kotlin.system.exitProcess

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
 * Force collection (so the cleaner releases every unreachable wrapper and
 * iterator) until the producer's leak counters all read zero: 0 objects,
 * 1 callbacks, 2 iterators, 3 cancel tokens, 4 returned allocations.
 */
fun expectNoLeaks(live: (Int) -> Long) {
    val kinds = listOf("objects", "callbacks", "iterators", "cancel tokens", "allocations")
    for (i in 0 until 400) {
        if (kinds.indices.all { live(it) == 0L }) return
        System.gc()
        System.runFinalization()
        Thread.sleep(5)
    }
    val counts = kinds.indices.joinToString { "${kinds[it]}=${live(it)}" }
    expect(false, "native resources still live at exit: $counts")
}
