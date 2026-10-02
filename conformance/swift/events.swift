// Conformance consumer: events sample, Swift target.
//
// Binds through the generated `Events` package and drives the `Subscriber`
// callback interface implemented as a Swift class (every method observed
// with its arguments, including the `EventBus` object the producer hands to
// `onAttached`), `Delivery` return values steering `publish`'s accepted
// count, the reference-counted `EventBus` object, the `publishLater` async
// wrapper, `messages()` as a lazy Sequence, the `lastMessage()` optional
// record, and the free function `routeOnce`. Release is observed through
// weak references.
//
// `route` is the `Result`-returning callback method: a subscriber that
// throws from it makes the producer fail the call with the foreign code -4.
// The events module declares no error domain, so `publish` doesn't throw and
// the failure stops the process with a message naming the code; a child
// process (this executable run with `trap-route`) proves that. At exit,
// every native object, callback, iterator, and allocation has been released.

import CEvents
import Events
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

/// Every live-object counter the producer keeps must settle at zero.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<200 {
        live = (0..<5).map { events_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(10_000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

struct RouteRejected: Error, LocalizedError {
    let topic: String
    var errorDescription: String? { "route rejected \(topic)" }
}

/// A consumer-side subscriber. Records every callback so the test can assert
/// the arguments the producer passed, and counts live instances so the
/// producer's `free` (which releases the generated box, and with it the last
/// strong reference to this object) is observable. Callbacks arrive on the
/// calling thread here, so plain mutable state is fine.
final class TestSubscriber: Subscriber, @unchecked Sendable {
    static var live = 0

    let skipTopic: String
    let failTopic: String
    let keepBus: Bool
    var routed: [String] = []
    var received: [Message] = []
    var attachedCount = 0
    /// Subscriber count observed through the bus object inside `onAttached`.
    var countSeenOnAttach: Int64 = -1
    /// The bus adopted from `onAttached` when `keepBus` is set.
    var attachedBus: EventBus?
    /// A weak view of the bus wrapper handed to `onAttached`.
    weak var weakAttachedBus: EventBus?

    init(skipTopic: String = "", failTopic: String = "", keepBus: Bool = false) {
        self.skipTopic = skipTopic
        self.failTopic = failTopic
        self.keepBus = keepBus
        TestSubscriber.live += 1
    }

    deinit { TestSubscriber.live -= 1 }

    func route(topic: String) throws -> Delivery {
        routed.append(topic)
        if topic == failTopic { throw RouteRejected(topic: topic) }
        if topic == skipTopic { return .skip }
        if topic == "stop" { return .acceptAndStop }
        return .accept
    }

    func onMessage(message: Message) throws -> Int64 {
        received.append(message)
        return Int64(received.count)
    }

    func onAttached(bus: EventBus) throws {
        attachedCount += 1
        // The object is live and usable inside the callback: `subscribe`
        // attaches before it pushes, so the count excludes this subscriber.
        countSeenOnAttach = bus.subscriberCount()
        weakAttachedBus = bus
        if keepBus { attachedBus = bus }
    }
}

/// Child-process mode: a subscriber whose `route` throws makes the
/// non-throwing `publish` stop the process.
if CommandLine.arguments.dropFirst().first == "trap-route" {
    let bus = EventBus()
    _ = bus.subscribe(subscriber: TestSubscriber(failTopic: "boom"))
    _ = bus.publish(topic: "boom", text: "never delivered", tags: [])
    print("publish returned after a failing route")
    exit(0)
}

func runTrapRouteChild() {
    let child = Process()
    child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
    child.arguments = ["trap-route"]
    let stderr = Pipe()
    child.standardError = stderr
    child.standardOutput = Pipe()
    do {
        try child.run()
    } catch {
        fail("could not start the trap-route child: \(error)")
    }
    let output = String(decoding: stderr.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    child.waitUntilExit()
    expect(child.terminationReason == .uncaughtSignal,
           "a throwing route stops the process (status \(child.terminationStatus))")
    expect(output.contains("failed with code -4: route rejected boom"),
           "the fatal error names the foreign code and message (got \(output))")
}

func run() async {
    // --- Empty bus -----------------------------------------------------------
    var bus: EventBus? = EventBus()
    expect(bus!.subscriberCount() == 0, "new bus has no subscribers")
    expect(bus!.lastMessage() == nil, "new bus has no last message")
    expect(Array(bus!.messages()).isEmpty, "new bus has no messages")

    // --- subscribe: onAttached receives a usable bus object ------------------
    var a: TestSubscriber? = TestSubscriber(skipTopic: "quiet")
    var b: TestSubscriber? = TestSubscriber(keepBus: true)
    weak var weakA = a
    weak var weakB = b

    expect(bus!.subscribe(subscriber: a!) == 1, "first subscribe returns 1")
    expect(a!.attachedCount == 1, "a attached once")
    expect(a!.countSeenOnAttach == 0, "a saw an empty bus on attach (got \(a!.countSeenOnAttach))")
    expect(a!.weakAttachedBus == nil, "bus wrapper handed to a was released after onAttached")

    expect(bus!.subscribe(subscriber: b!) == 2, "second subscribe returns 2")
    expect(b!.countSeenOnAttach == 1, "b saw one subscriber on attach (got \(b!.countSeenOnAttach))")
    expect(b!.attachedBus != nil && b!.weakAttachedBus != nil, "b kept the adopted bus")
    expect(bus!.subscriberCount() == 2, "subscriberCount == 2")

    // --- publish: Delivery steers the accepted count -------------------------
    expect(bus!.publish(topic: "news", text: "hello", tags: ["x", "y"]) == 2,
           "both subscribers accept 'news'")
    expect(a!.routed == ["news"] && b!.routed == ["news"], "route asked once per subscriber")
    let first = a!.received[0]
    expect(first == Message(seq: 1, topic: "news", text: "hello", tags: ["x", "y"]),
           "first message (got \(first))")

    // a skips 'quiet'; b accepts it.
    expect(bus!.publish(topic: "quiet", text: "psst", tags: []) == 1, "skip lowers the accepted count")
    expect(a!.received.count == 1, "a did not receive the skipped message")
    expect(b!.received.count == 2 && b!.received[1].seq == 2 && b!.received[1].tags.isEmpty,
           "b received the second message with empty tags")

    // AcceptAndStop from the first subscriber stops delivery to the rest.
    let c = TestSubscriber()
    expect(bus!.subscribe(subscriber: c) == 3, "third subscribe returns 3")
    expect(bus!.publish(topic: "stop", text: "last", tags: ["z"]) == 1, "acceptAndStop delivers once")
    expect(a!.received.count == 2 && a!.received[1].text == "last", "a took the stop message")
    expect(b!.routed == ["news", "quiet"], "b was not asked after the stop (got \(b!.routed))")
    expect(c.routed.isEmpty && c.received.isEmpty, "c was never reached")

    // --- messages(): iterator as a lazy Sequence -----------------------------
    expect(Array(bus!.messages()) == ["hello", "psst", "last"], "messages in order")
    var joined: [String] = []
    for m in bus!.messages() { joined.append(m.uppercased()) }
    expect(joined == ["HELLO", "PSST", "LAST"], "for-in over the sequence")
    // Abandoning early releases the producer iterator through deinit.
    expect(bus!.messages().first(where: { $0.hasPrefix("p") }) == "psst", "early exit")

    // --- lastMessage(): optional record --------------------------------------
    expect(bus!.lastMessage() == Message(seq: 3, topic: "stop", text: "last", tags: ["z"]),
           "lastMessage fields")

    // --- publishLater: async wrapper resumed from a producer thread ----------
    let later = await bus!.publishLater(topic: "news", text: "later")
    expect(later == 3, "publishLater accepted by all three (got \(later))")
    expect(bus!.lastMessage()?.seq == 4, "async publish took seq 4")
    expect(c.received.count == 1, "c received the async message")

    // --- The bus adopted in onAttached is the same object --------------------
    let kept = b!.attachedBus!
    expect(kept.publish(topic: "via-kept", text: "same object", tags: []) == 3,
           "publish through the kept wrapper reaches every subscriber")
    expect(bus!.lastMessage()?.topic == "via-kept", "publish via kept wrapper visible through the original")

    // --- routeOnce: a callback passed to a free function is freed on return --
    weak var weakD: TestSubscriber?
    do {
        let d = TestSubscriber(skipTopic: "quiet")
        weakD = d
        expect(Events.routeOnce(subscriber: d, topic: "quiet") == .skip, "routeOnce skip")
        expect(Events.routeOnce(subscriber: d, topic: "stop") == .acceptAndStop, "routeOnce acceptAndStop")
        expect(Events.routeOnce(subscriber: d, topic: "other") == .accept, "routeOnce accept")
        expect(d.attachedCount == 0, "routeOnce never attaches")
    }
    expect(weakD == nil, "routeOnce released its subscriber (free ran)")

    // --- clearSubscribers releases every subscriber --------------------------
    a = nil
    b = nil
    expect(weakA != nil && weakB != nil, "the bus retains a and b after the consumer drops them")
    bus!.clearSubscribers()
    expect(weakA == nil, "a freed by clearSubscribers")
    expect(weakB == nil, "b (and the bus reference it kept) freed by clearSubscribers")
    expect(bus!.publish(topic: "news", text: "nobody", tags: []) == 0, "publish with no subscribers")

    // --- Destroying the bus frees a retained subscriber ----------------------
    weak var weakE: TestSubscriber?
    weak var weakBus2: EventBus?
    do {
        let bus2 = EventBus()
        weakBus2 = bus2
        let e = TestSubscriber()
        weakE = e
        expect(bus2.subscribe(subscriber: e) == 1, "bus2 subscribe")
    }
    expect(weakBus2 == nil, "bus2 wrapper deinitialized")
    expect(weakE == nil, "dropping the last bus reference freed its subscriber")

    bus = nil
    expect(TestSubscriber.live == 1, "only c is still alive (got \(TestSubscriber.live))")
}

await run()
runTrapRouteChild()
expect(TestSubscriber.live == 0, "every subscriber was released (got \(TestSubscriber.live))")
assertNoLeaks()
print("swift/events: OK")
