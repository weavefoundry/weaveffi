package {{PACKAGE}}

import com.sun.jna.Pointer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.cancellation.CancellationException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Bridges one native async call to a suspended coroutine. The library calls
 * the call's completion exactly once, from whichever thread completes it, and
 * [PendingCalls] hands the outcome here. [lift] turns the raw C result into
 * the public value (and releases what it owns).
 */
internal class NativeCompletion<T>(
    private val cont: CancellableContinuation<T>,
    private val domain: Int,
    private val lift: (Any?) -> T,
    cancellable: Boolean,
) {
    // The native cancel token, or null when the call isn't cancellable or has
    // completed. Guarded by `this` so a late cancel never touches a freed
    // token; a completion that lands inside `cancel` (on the same thread)
    // defers the release until the cancel returns.
    private var token: Pointer? = if (cancellable) Lib.c.{{PREFIX}}_cancel_token_create() else null
    private var cancelling = false
    private var released = false

    val cancelToken: Pointer?
        get() = synchronized(this) { token }

    fun cancel() {
        synchronized(this) {
            val t = token ?: return
            cancelling = true
            try {
                Lib.c.{{PREFIX}}_cancel_token_cancel(t)
            } finally {
                cancelling = false
            }
            if (released) destroyToken()
        }
    }

    fun releaseToken() {
        synchronized(this) {
            if (cancelling) released = true else destroyToken()
        }
    }

    private fun destroyToken() {
        token?.let { Lib.c.{{PREFIX}}_cancel_token_destroy(it) }
        token = null
    }

    /** The call completed: with the boxed error [err], or with [result]. */
    fun complete(err: Pointer?, result: Any?) {
        releaseToken()
        if (err != null) {
            val error = NativeError(err)
            val code = error.code
            val message = error.message()
            val payload = error.payload()
            Lib.c.{{PREFIX}}_error_free(err)
            cont.resumeWithException(
                if (code == -5) CancellationException(message.ifEmpty { "cancelled" })
                else nativeError(domain, code, message, payload),
            )
            return
        }
        val value = try {
            lift(result)
        } catch (t: Throwable) {
            cont.resumeWithException(t)
            return
        }
        cont.resume(value)
    }
}

/**
 * The async calls in flight, by the context pointer each passes the
 * library. The completion callbacks (one static instance per result type,
 * so they stay reachable for as long as the library can call them) look the
 * call up here.
 */
internal object PendingCalls {
    private val calls = ConcurrentHashMap<Long, NativeCompletion<*>>()
    private val ids = AtomicLong()

    fun add(call: NativeCompletion<*>): Pointer {
        val id = ids.incrementAndGet()
        calls[id] = call
        return Pointer(id)
    }

    fun remove(context: Pointer) {
        calls.remove(Pointer.nativeValue(context))
    }

    /** A completion fired: hands its outcome to the call's coroutine. */
    fun complete(context: Pointer?, err: Pointer?, result: Any?) {
        try {
            val call = context?.let { calls.remove(Pointer.nativeValue(it)) }
            if (call != null) {
                call.complete(err, result)
            } else {
                Lib.c.{{PREFIX}}_error_free(err)
                if (result is Run) freeRun(result)
            }
        } catch (ignored: Throwable) {
            // Nothing may unwind into the library.
        }
    }
}

/**
 * Starts a native async call and suspends until it completes. [launch] gets
 * the cancel token (null unless [cancellable]) and the context the call's
 * completion carries. Cancelling the coroutine cancels the native call; a
 * cancelled call (code -5) resumes with [CancellationException].
 */
internal suspend fun <T> awaitNative(
    cancellable: Boolean,
    domain: Int,
    lift: (Any?) -> T,
    launch: (Pointer?, Pointer) -> Unit,
): T = suspendCancellableCoroutine { cont ->
    val completion = NativeCompletion(cont, domain, lift, cancellable)
    val context = PendingCalls.add(completion)
    cont.invokeOnCancellation { completion.cancel() }
    try {
        launch(completion.cancelToken, context)
    } catch (t: Throwable) {
        PendingCalls.remove(context)
        completion.releaseToken()
        throw t
    }
}
