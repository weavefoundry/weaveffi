// Conformance consumer: events sample, Swift target.
//
// Exercises the context-boxed listener trampoline (register pins the closure
// via Unmanaged, the producer fires it synchronously on send, unregister
// releases it and stops delivery) and the opaque-iterator ABI behind
// getMessages. The events module declares no error domain and no function
// throws, so every call is made without `try` (failures would trap). Exits
// non-zero on any mismatch.

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

let recorder = Recorder()
let sub = Events.registerMessageListener { message in
    recorder.received.append(message)
}
expect(sub > 0, "listener id positive")

Events.sendMessage(text: "alpha")
Events.sendMessage(text: "beta")
expect(recorder.received == ["alpha", "beta"],
       "listener received sends (got \(recorder.received))")

// getMessages returns a lazy single-pass Sequence: one producer `next` per
// consumer step, handle destroyed on exhaustion (or by deinit if abandoned).
let msgs = Array(Events.getMessages())
expect(msgs == ["alpha", "beta"],
       "iterator yields messages in order (got \(msgs))")

// Unregister stops delivery; the producer still records the message.
Events.unregisterMessageListener(sub)
Events.sendMessage(text: "gamma")
expect(recorder.received == ["alpha", "beta"],
       "no delivery after unregister (got \(recorder.received))")
expect(Array(Events.getMessages()).count == 3, "producer kept recording")

// Abandoning the sequence early releases the handle through deinit.
let firstOnly = Events.getMessages().first(where: { _ in true })
expect(firstOnly == "alpha", "early stop yields only the first message")

print("swift/events: OK")
