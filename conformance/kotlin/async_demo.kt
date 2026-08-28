// Conformance consumer: async-demo sample, Android/Kotlin (JNI) target.
//
// Drives the suspend-function async surface end to end: `runTask` resumed
// through a WeaveContinuation from the producer's worker thread and decoded
// from a value buffer into the TaskResult data class, the typed
// TaskException.InvalidName thrown for an empty name, the buffered
// list-of-records round trip through `runBatch`, the direct-scalar
// `runNTasks`, the sync `cancelTask`, and `activeCallbacks` settling to zero
// once every task body has completed. Compiled in-module with the generated
// `WeaveFFI.kt`; the JNI bridge loads as `libweaveffi` from java.library.path.
@file:JvmName("Main")

import com.weaveffi.TaskException
import com.weaveffi.WeaveFFI
import com.weaveffi.WeaveFFIException
import kotlinx.coroutines.runBlocking
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

fun main() = runBlocking {
    // Async record return: the suspend fun resumes with a TaskResult decoded
    // from the completion callback's value buffer.
    val result = WeaveFFI.runTask("alpha")
    expect(result.id > 0, "runTask assigns an id")
    expect(result.value == "completed: alpha", "runTask value (got ${result.value})")
    expect(result.success, "runTask success flag")

    // Typed async error: the empty name resumes by throwing the typed
    // subclass carrying its stable code.
    try {
        WeaveFFI.runTask("")
        expect(false, "expected TaskException.InvalidName for empty name")
    } catch (e: TaskException.InvalidName) {
        expect(e.code == 1, "InvalidName carries code 1 (got ${e.code})")
        expect(e is TaskException, "subclass of TaskException")
        expect(e is WeaveFFIException, "subclass of the brand exception")
    }

    // Buffered list-of-records both ways.
    val batch = WeaveFFI.runBatch(listOf("a", "b", "c"))
    expect(
        batch.map { it.value } == listOf("completed: a", "completed: b", "completed: c"),
        "runBatch values (got ${batch.map { it.value }})",
    )
    expect(batch.all { it.success }, "runBatch success flags")

    // Direct scalar through the async callback.
    expect(WeaveFFI.runNTasks(7) == 7, "runNTasks echoes n")

    // Sync functions beside the async ones.
    expect(!WeaveFFI.cancelTask(1L), "cancelTask reports not cancelled")

    // Every spawned task body has completed by the time its callback fires.
    expect(WeaveFFI.activeCallbacks() == 0L, "activeCallbacks settles to zero")

    println("kotlin async-demo conformance: OK")
}
