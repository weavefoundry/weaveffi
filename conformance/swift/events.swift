// Conformance consumer: events sample, Swift target.
//
// Binds through the generated `Events` module and drives the ABI 2 surface:
// the `Subscriber` callback interface implemented as a Swift class (every
// method observed with its arguments, including the `EventBus` object the
// producer hands to `onAttached`, which is usable and released), `Delivery`
// return values steering `publish`'s accepted count, the reference-counted
// `EventBus` object, the `publishLater` async wrapper, `messages()` as a lazy
// Sequence, the `lastMessage()` optional record, and the free function
// `routeOnce`. Release is observed through weak references: a subscriber's
// box is freed when the bus drops it, and the bus wrapper handed to a callback
// is destroyed when the consumer lets go of it. The events module declares no
// error domain, so every wrapper is non-throwing and traps on a failure
// (including a subscriber that throws, which the producer reports as code
// -4); the throwing-callback path is exercised in the kvstore lane, whose
// wrappers do throw. Exits non-zero on any mismatch.

import Foundation
import Events

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

/// A consumer-side subscriber. Records every callback so the test can assert
/// the arguments the producer passed, and counts live instances so the
/// producer's `free` (which releases the generated box, and with it the last
/// strong reference to this object) is observable.
final class TestSubscriber: Subscriber {
    static var live = 0

    let name: String
    let skipTopic: String
    let keepBus: Bool
    var routed: [String] = []
    var received: [Message] = []
    var attachedCount = 0
    /// Subscriber count observed through the bus object inside `onAttached`.
    var countSeenOnAttach: Int64 = -1
    /// The bus adopted from `onAttached` when `keepBus` is set.
    var attachedBus: EventBus?
    /// A weak view of the bus wrapper handed to `onAttached`, to prove the
    /// wrapper (and its strong reference) was released when not kept.
    weak var weakAttachedBus: EventBus?

    init(name: String, skipTopic: String = "", keepBus: Bool = false) {
        self.name = name
        self.skipTopic = skipTopic
        self.keepBus = keepBus
        TestSubscriber.live += 1
    }

    deinit { TestSubscriber.live -= 1 }

    func route(topic: String) throws -> Delivery {
        routed.append(topic)
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

// --- Empty bus ---------------------------------------------------------------
var bus: EventBus? = EventBus()
expect(bus!.subscriberCount() == 0, "new bus has no subscribers")
expect(bus!.lastMessage() == nil, "new bus has no last message")
expect(Array(bus!.messages()).isEmpty, "new bus has no messages")

// --- subscribe: onAttached receives a usable bus object ----------------------
var a: TestSubscriber? = TestSubscriber(name: "a", skipTopic: "quiet")
var b: TestSubscriber? = TestSubscriber(name: "b", keepBus: true)
weak var weakA = a
weak var weakB = b

expect(bus!.subscribe(subscriber: a!) == 1, "first subscribe returns 1")
expect(a!.attachedCount == 1, "a attached once")
expect(a!.countSeenOnAttach == 0, "a saw an empty bus on attach (got \(a!.countSeenOnAttach))")
expect(a!.weakAttachedBus == nil, "bus wrapper handed to a was released after onAttached")
expect(bus!.subscriberCount() == 1, "bus still alive after callback released its reference")

expect(bus!.subscribe(subscriber: b!) == 2, "second subscribe returns 2")
expect(b!.attachedCount == 1, "b attached once")
expect(b!.countSeenOnAttach == 1, "b saw one subscriber on attach (got \(b!.countSeenOnAttach))")
expect(b!.attachedBus != nil && b!.weakAttachedBus != nil, "b kept the adopted bus")
expect(bus!.subscriberCount() == 2, "subscriberCount == 2")

// --- publish: Delivery steers the accepted count -----------------------------
expect(bus!.publish(topic: "news", text: "hello", tags: ["x", "y"]) == 2,
       "both subscribers accept 'news'")
expect(a!.routed == ["news"] && b!.routed == ["news"], "route asked once per subscriber")
expect(a!.received.count == 1 && b!.received.count == 1, "one delivery each")
let first = a!.received[0]
expect(first.seq == 1, "seq starts at 1 (got \(first.seq))")
expect(first.topic == "news", "topic (got \(first.topic))")
expect(first.text == "hello", "text (got \(first.text))")
expect(first.tags == ["x", "y"], "tags (got \(first.tags))")

// a skips 'quiet'; b accepts it.
expect(bus!.publish(topic: "quiet", text: "psst", tags: []) == 1, "skip lowers the accepted count")
expect(a!.routed == ["news", "quiet"], "a was asked about 'quiet' (got \(a!.routed))")
expect(a!.received.count == 1, "a did not receive the skipped message")
expect(b!.received.count == 2 && b!.received[1].seq == 2 && b!.received[1].tags.isEmpty,
       "b received the second message with empty tags")

// AcceptAndStop from the first subscriber stops delivery to the rest.
let c = TestSubscriber(name: "c")
expect(bus!.subscribe(subscriber: c) == 3, "third subscribe returns 3")
expect(bus!.publish(topic: "stop", text: "last", tags: ["z"]) == 1, "acceptAndStop delivers once")
expect(a!.received.count == 2 && a!.received[1].text == "last", "a took the stop message")
expect(b!.routed == ["news", "quiet"], "b was not asked after the stop (got \(b!.routed))")
expect(c.routed.isEmpty && c.received.isEmpty, "c was never reached")

// --- messages(): iterator as a lazy Sequence ---------------------------------
let texts = Array(bus!.messages())
expect(texts == ["hello", "psst", "last"], "messages in order (got \(texts))")
var joined: [String] = []
for m in bus!.messages() { joined.append(m.uppercased()) }
expect(joined == ["HELLO", "PSST", "LAST"], "for-in over the sequence")
expect(bus!.messages().map { $0.count } == [5, 4, 4], "map over the sequence")
// Abandoning early releases the producer iterator through deinit.
expect(bus!.messages().first(where: { $0.hasPrefix("p") }) == "psst", "early exit")

// --- lastMessage(): optional record ------------------------------------------
let last = bus!.lastMessage()
expect(last != nil, "lastMessage present")
expect(last!.seq == 3 && last!.topic == "stop" && last!.text == "last" && last!.tags == ["z"],
       "lastMessage fields (got \(last!))")

// --- publishLater: async wrapper awaited from the producer's thread ---------
let later = await bus!.publishLater(topic: "news", text: "later")
expect(later == 3, "publishLater accepted by all three (got \(later))")
expect(bus!.lastMessage()?.text == "later", "async publish logged")
expect(bus!.lastMessage()?.seq == 4, "async publish took seq 4")
expect(c.received.count == 1 && c.received[0].tags.isEmpty, "c received the async message")

// --- The bus adopted in onAttached is the same object --------------------------
let kept = b!.attachedBus!
expect(kept.subscriberCount() == 3, "kept bus sees all three subscribers")
expect(kept.publish(topic: "via-kept", text: "same object", tags: []) == 3,
       "publish through the kept wrapper reaches every subscriber")
expect(bus!.lastMessage()?.topic == "via-kept", "publish via kept wrapper visible through the original")
expect(Array(bus!.messages()).count == 5, "five messages logged")

// --- routeOnce: a callback passed to a free function is freed on return -------
weak var weakD: TestSubscriber?
do {
    let d = TestSubscriber(name: "d", skipTopic: "quiet")
    weakD = d
    expect(Events.routeOnce(subscriber: d, topic: "quiet") == .skip, "routeOnce skip")
    expect(Events.routeOnce(subscriber: d, topic: "stop") == .acceptAndStop, "routeOnce acceptAndStop")
    expect(Events.routeOnce(subscriber: d, topic: "other") == .accept, "routeOnce accept")
    expect(d.routed == ["quiet", "stop", "other"], "routeOnce asked route each time")
    expect(d.attachedCount == 0, "routeOnce never attaches")
}
expect(weakD == nil, "routeOnce released its subscriber (free ran)")

// --- clearSubscribers releases every subscriber ------------------------------
a = nil
b = nil
expect(weakA != nil && weakB != nil, "the bus retains a and b after the consumer drops them")
bus!.clearSubscribers()
expect(bus!.subscriberCount() == 0, "no subscribers after clear")
expect(weakA == nil, "a freed by clearSubscribers")
expect(weakB == nil, "b (and the bus reference it kept) freed by clearSubscribers")
expect(bus!.lastMessage()?.topic == "via-kept", "bus still alive after b's kept reference was released")
expect(bus!.publish(topic: "news", text: "nobody", tags: []) == 0, "publish with no subscribers")
expect(c.received.count == 2, "c is no longer delivered to (got \(c.received.count))")

// --- Destroying the bus frees a retained subscriber --------------------------
weak var weakE: TestSubscriber?
weak var weakBus2: EventBus?
do {
    let bus2 = EventBus()
    weakBus2 = bus2
    let e = TestSubscriber(name: "e")
    weakE = e
    expect(bus2.subscribe(subscriber: e) == 1, "bus2 subscribe")
    expect(bus2.publish(topic: "t", text: "m", tags: []) == 1, "bus2 publish")
}
expect(weakBus2 == nil, "bus2 wrapper deinitialized")
expect(weakE == nil, "dropping the last bus reference freed its subscriber")

bus = nil
expect(TestSubscriber.live == 1, "only c is still alive (got \(TestSubscriber.live))")

print("swift/events: OK")
