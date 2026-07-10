// Conformance consumer: events sample, Android/Kotlin (JNI) target.
//
// Exercises the JNI listener trampoline (register pins the Kotlin lambda with
// a GlobalRef, the producer fires it synchronously on send via FunctionN.invoke,
// unregister releases it and stops delivery) and the iterator-backed
// getMessages drained through Kotlin's Iterator. Function names are the 0.5.0
// defaults: lowerCamelCase with the module prefix stripped.
@file:JvmName("Main")

import com.weaveffi.WeaveFFI
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

fun main() {
    val received = mutableListOf<String>()
    val sub = WeaveFFI.registerMessageListener { message -> received.add(message) }
    expect(sub > 0L, "listener id positive")

    WeaveFFI.sendMessage("alpha")
    WeaveFFI.sendMessage("beta")
    expect(received == listOf("alpha", "beta"), "listener received sends (got $received)")

    val msgs = mutableListOf<String>()
    val it = WeaveFFI.getMessages()
    while (it.hasNext()) msgs.add(it.next())
    expect(msgs == listOf("alpha", "beta"), "iterator yields messages in order (got $msgs)")

    // Unregister stops delivery; the producer still records the message.
    WeaveFFI.unregisterMessageListener(sub)
    WeaveFFI.sendMessage("gamma")
    expect(received == listOf("alpha", "beta"), "no delivery after unregister (got $received)")
    val after = mutableListOf<String>()
    val it2 = WeaveFFI.getMessages()
    while (it2.hasNext()) after.add(it2.next())
    expect(after.size == 3, "producer kept recording (got $after)")

    println("kotlin/events: OK")
}
