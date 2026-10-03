// Conformance consumer: async-demo sample, Kotlin (JVM via JNI) target.
//
// Drives the suspend-function async surface end to end: `runTask` resumed
// through a `NativeCompletion` from the producer's worker thread and decoded
// from a value buffer into the `TaskResult` data class, the typed
// `TaskException.InvalidName` for an empty name, the buffered list-of-records
// round trip through `runBatch`, the direct-scalar `runNTasks`, and the
// cancellable `wait`: it completes normally when left alone, and cancelling
// the awaiting coroutine (directly or through `withTimeout`) cancels the
// native token, so the producer drops the pending work long before its
// timeout. At exit it checks that every native resource was released.
@file:JvmName("Main")

import async_demo.FfiException
import async_demo.JniBridge
import async_demo.TaskException
import async_demo.Tasks
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

/** Poll `activeCallbacks` until it reads zero or `ms` milliseconds pass. */
fun settled(ms: Long): Boolean {
    val deadline = System.currentTimeMillis() + ms
    while (System.currentTimeMillis() < deadline) {
        if (Tasks.activeCallbacks() == 0L) return true
        Thread.sleep(5)
    }
    return Tasks.activeCallbacks() == 0L
}

fun run() = runBlocking {
    // Async record return: the suspend fun resumes with a TaskResult decoded
    // from the completion's value buffer.
    val result = Tasks.runTask("alpha")
    expect(result.id > 0, "runTask assigns an id")
    expect(result.value == "completed: alpha", "runTask value (got ${result.value})")
    expect(result.success, "runTask success flag")

    // Typed async error: the empty name throws the typed subclass.
    val invalid = try {
        Tasks.runTask("")
        null
    } catch (e: TaskException.InvalidName) {
        e
    }
    expect(invalid != null, "expected TaskException.InvalidName for an empty name")
    expect(invalid!!.code == 1, "InvalidName carries code 1 (got ${invalid.code})")
    expect(invalid is FfiException, "InvalidName is an FfiException")

    // Buffered list-of-records both ways.
    val batch = Tasks.runBatch(listOf("a", "b", "c"))
    expect(
        batch.map { it.value } == listOf("completed: a", "completed: b", "completed: c"),
        "runBatch values (got ${batch.map { it.value }})",
    )
    expect(batch.all { it.success }, "runBatch success flags")

    // Direct scalar through the async completion, many at once.
    expect(Tasks.runNTasks(7) == 7, "runNTasks echoes n")
    val many = (1..50).map { n -> async { Tasks.runNTasks(n) } }.map { it.await() }
    expect(many == (1..50).toList(), "concurrent runNTasks")

    // A cancellable call left alone completes normally.
    expect(Tasks.wait(20L) == 20L, "wait(20) completes with 20")

    // Cancelling the awaiting coroutine cancels the native token: the call
    // ends with CancellationException at once, and the producer drops the
    // pending future (and its in-flight guard) instead of waiting 10 s.
    val started = System.currentTimeMillis()
    val job = async { Tasks.wait(10_000L) }
    delay(50)
    expect(Tasks.activeCallbacks() == 1L, "wait is in flight (got ${Tasks.activeCallbacks()})")
    job.cancel()
    val cancelled = try {
        job.await()
        null
    } catch (e: CancellationException) {
        e
    }
    expect(cancelled != null, "a cancelled wait throws CancellationException")
    expect(settled(2_000), "the producer dropped the cancelled wait (active ${Tasks.activeCallbacks()})")
    expect(System.currentTimeMillis() - started < 5_000, "cancellation didn't wait for the timeout")

    // withTimeout cancels the same way.
    val timedOut = try {
        withTimeout(50) { Tasks.wait(10_000L) }
        null
    } catch (e: TimeoutCancellationException) {
        e
    }
    expect(timedOut != null, "withTimeout cancels wait")
    expect(settled(2_000), "the producer dropped the timed-out wait")
    expect(System.currentTimeMillis() - started < 5_000, "timeouts didn't wait for the producer's timer")

    // Every spawned task body has completed by the time its completion fires.
    expect(Tasks.activeCallbacks() == 0L, "activeCallbacks settles to zero")
}

fun main() {
    run()
    println("kotlin/async-demo: OK")
    expectNoLeaks { JniBridge.debug_live(it) }
    println("kotlin/async-demo: no leaks")
}
