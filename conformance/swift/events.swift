// Conformance consumer: events sample, Swift target.
//
// Exercises the context-boxed listener trampoline (register pins the closure
// via Unmanaged, the producer fires it synchronously on send, unregister
// releases it and stops delivery) and the opaque-iterator ABI behind
// events_get_messages. Exits non-zero on any mismatch.

import Foundation
import Events

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

final class Recorder {
    var received: [String] = []
}

do {
    let recorder = Recorder()
    let sub = Events.events_register_message_listener { message in
        recorder.received.append(message)
    }
    expect(sub > 0, "listener id positive")

    try Events.events_send_message("alpha")
    try Events.events_send_message("beta")
    expect(recorder.received == ["alpha", "beta"],
           "listener received sends (got \(recorder.received))")

    let msgs = try Events.events_get_messages()
    expect(msgs == ["alpha", "beta"],
           "iterator yields messages in order (got \(msgs))")

    // Unregister stops delivery; the producer still records the message.
    Events.events_unregister_message_listener(sub)
    try Events.events_send_message("gamma")
    expect(recorder.received == ["alpha", "beta"],
           "no delivery after unregister (got \(recorder.received))")
    expect(try Events.events_get_messages().count == 3, "producer kept recording")

    print("swift/events: OK")
} catch {
    fail("unexpected error: \(error)")
}
