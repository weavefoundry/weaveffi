// Conformance consumer: events sample, Android/Kotlin (JNI) target.
//
// Exercises the JNI listener trampoline (register pins the Kotlin lambda with
// a GlobalRef, the producer fires it synchronously on send via FunctionN.invoke,
// unregister releases it and stops delivery) and the iterator-backed
// events_get_messages drained through Kotlin's Iterator.
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
    val sub = WeaveFFI.events_register_message_listener { message -> received.add(message) }
    expect(sub > 0L, "listener id positive")

    WeaveFFI.events_send_message("alpha")
    WeaveFFI.events_send_message("beta")
    expect(received == listOf("alpha", "beta"), "listener received sends (got $received)")

    val msgs = mutableListOf<String>()
    val it = WeaveFFI.events_get_messages()
    while (it.hasNext()) msgs.add(it.next())
    expect(msgs == listOf("alpha", "beta"), "iterator yields messages in order (got $msgs)")

    // Unregister stops delivery; the producer still records the message.
    WeaveFFI.events_unregister_message_listener(sub)
    WeaveFFI.events_send_message("gamma")
    expect(received == listOf("alpha", "beta"), "no delivery after unregister (got $received)")
    val after = mutableListOf<String>()
    val it2 = WeaveFFI.events_get_messages()
    while (it2.hasNext()) after.add(it2.next())
    expect(after.size == 3, "producer kept recording (got $after)")

    println("kotlin/events: OK")
}
