// Async stress smoke test for the Android consumer.
//
// Mirrors the JNI-side async pattern that the Android generator emits:
// the JNI shim wraps a Kotlin continuation in a `WeaveContinuation<T>`
// and pins it via `NewGlobalRef`, releasing it with `DeleteGlobalRef`
// from the C callback once the suspend point resumes.
//
// We intentionally avoid actually loading the native library because CI
// does not run on-device; this file is only consumed by `kotlinc -d`
// during the end-to-end tests, mirroring `Main.kt`.
package com.weaveffi.example

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

internal class WeaveContinuation<T>(
    private val cont: kotlinx.coroutines.CancellableContinuation<T>,
) {
    @Suppress("UNCHECKED_CAST")
    fun onSuccess(result: Any?) {
        cont.resume(result as T)
    }

    fun onError(message: String) {
        cont.resumeWithException(RuntimeException(message))
    }
}

object AsyncDemo {
    init { System.loadLibrary("async_demo_jni") }

    @JvmStatic external fun runNTasksAsync(n: Int, callback: Any)

    @JvmStatic external fun activeCallbacks(): Long

    @JvmStatic
    suspend fun runNTasks(n: Int): Int = suspendCancellableCoroutine { cont ->
        runNTasksAsync(n, WeaveContinuation(cont))
    }
}

fun main() {
    val nTasks = 1000
    runBlocking {
        val results = (0 until nTasks)
            .map { i -> async { AsyncDemo.runNTasks(i) } }
            .awaitAll()
        for (i in 0 until nTasks) {
            require(results[i] == i) {
                "results[$i] = ${results[i]}, expected $i"
            }
        }
        require(AsyncDemo.activeCallbacks() == 0L) {
            "active_callbacks = ${AsyncDemo.activeCallbacks()} (expected 0)"
        }
    }
    println("OK")
}
