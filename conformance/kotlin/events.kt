// Conformance consumer: events sample, Kotlin (JVM via JNI) target.
//
// Exercises the callback-interface and object surface: `Subscriber` is a
// generated Kotlin interface the consumer implements (the JNI vtable
// trampolines pin the implementing object with a global reference and call
// the `JniBridge` dispatch shims from any producer thread), `EventBus` is a
// reference-counted `AutoCloseable` wrapper, and `Message` is a data class
// decoded from a value buffer. Asserts that every callback method is invoked
// with the right arguments (including the bus object handed to
// `onAttached`, which is usable and independently closeable), that
// `Delivery` return values steer `publish`'s accepted count, that a Kotlin
// exception thrown inside a callback (including `route`, which the producer
// declares as returning `Result<_, ForeignError>`) surfaces to the caller as
// `FfiException` code -4, the `suspend` `publishLater`, the
// `NativeIterator`-backed `messages()`, the nullable `lastMessage()`,
// `Events.routeOnce` without a bus, close semantics, and, at exit, that
// every native resource was released.
@file:JvmName("Main")

import events.Delivery
import events.EventBus
import events.Message
import events.Subscriber
import events.Events
import events.JniBridge
import events.FfiException
import java.lang.ref.WeakReference
import kotlinx.coroutines.runBlocking

/**
 * A subscriber that records every callback. `skipTopic` is routed as Skip,
 * `stopTopic` as AcceptAndStop, `failTopic` throws from `route`, and
 * `failOnMessage` throws from `onMessage` instead.
 */
class RecordingSubscriber(
    val name: String,
    private val skipTopic: String? = null,
    private val stopTopic: String? = null,
    private val failTopic: String? = null,
    private val failOnMessage: Boolean = false,
    private val failOnAttached: Boolean = false,
) : Subscriber {
    val routed = mutableListOf<String>()
    val messages = mutableListOf<Message>()
    var attachedCount = 0
    var subscriberCountAtAttach = -1L
    var keptBus: EventBus? = null

    override fun route(topic: String): Delivery {
        routed.add(topic)
        if (topic == failTopic) throw IllegalStateException("subscriber $name rejected $topic")
        return when (topic) {
            skipTopic -> Delivery.Skip
            stopTopic -> Delivery.AcceptAndStop
            else -> Delivery.Accept
        }
    }

    override fun onMessage(message: Message): Long {
        if (failOnMessage) throw RuntimeException("subscriber $name failed on message ${message.seq}")
        messages.add(message)
        return messages.size.toLong()
    }

    override fun onAttached(bus: EventBus) {
        attachedCount++
        if (failOnAttached) throw IllegalArgumentException("subscriber $name refused to attach")
        // The bus arrives as an adopted strong reference: it's usable right
        // here (subscribe hands it over before it takes its lock) and we keep
        // the wrapper so a later close proves it's independent of the caller's.
        subscriberCountAtAttach = bus.subscriberCount()
        keptBus = bus
    }
}

/** Route once through a subscriber nothing else references, returning a weak handle to it. */
fun routeOnceThrowaway(): WeakReference<RecordingSubscriber> {
    val sub = RecordingSubscriber("throwaway")
    expect(Events.routeOnce(sub, "t") == Delivery.Accept, "routeOnce throwaway")
    return WeakReference(sub)
}

/** Subscribe a subscriber nothing else references, returning a weak handle to it. */
fun subscribeThrowaway(bus: EventBus): WeakReference<RecordingSubscriber> {
    val sub = RecordingSubscriber("gone")
    expect(bus.subscribe(sub) == 4L, "subscribe gone")
    sub.keptBus!!.close()
    return WeakReference(sub)
}

fun run() {
    val bus = EventBus()
    expect(bus.subscriberCount() == 0L, "fresh bus has no subscribers")
    expect(bus.lastMessage() == null, "fresh bus has no last message")
    expect(bus.messages().asSequence().toList().isEmpty(), "fresh bus has no messages")
    expect(bus.publish("void", "nobody home", listOf()) == 0L, "publish with no subscribers accepts 0")

    // Subscribe three subscribers; each gets exactly one onAttached with the
    // bus, observing the count before it was added.
    val quiet = RecordingSubscriber("quiet", skipTopic = "quiet")
    val stopper = RecordingSubscriber("stopper", stopTopic = "stop")
    val tail = RecordingSubscriber("tail")
    expect(bus.subscribe(quiet) == 1L, "first subscribe returns 1")
    expect(bus.subscribe(stopper) == 2L, "second subscribe returns 2")
    expect(bus.subscribe(tail) == 3L, "third subscribe returns 3")
    expect(bus.subscriberCount() == 3L, "subscriberCount == 3")
    for ((i, s) in listOf(quiet, stopper, tail).withIndex()) {
        expect(s.attachedCount == 1, "${s.name} attached once (got ${s.attachedCount})")
        expect(
            s.subscriberCountAtAttach == i.toLong(),
            "${s.name} saw $i subscribers at attach (got ${s.subscriberCountAtAttach})"
        )
        expect(s.keptBus != null, "${s.name} kept the bus")
    }

    // Delivery steering: Accept everywhere -> 3; Skip for `quiet` -> 2;
    // AcceptAndStop in the middle -> 2 and `tail` never sees it.
    expect(bus.publish("news", "hello", listOf("a", "b")) == 2L + 1L, "publish news accepted by 3")
    expect(bus.publish("quiet", "psst", listOf()) == 2L, "publish quiet accepted by 2")
    expect(bus.publish("stop", "last", listOf("z")) == 2L, "publish stop accepted by 2")
    expect(quiet.routed == listOf("news", "quiet", "stop"), "quiet routed every topic (got ${quiet.routed})")
    expect(stopper.routed == listOf("news", "quiet", "stop"), "stopper routed every topic")
    expect(tail.routed == listOf("news", "quiet"), "tail never routed stop (got ${tail.routed})")
    expect(quiet.messages.map { it.text } == listOf("hello", "last"), "quiet got hello,last (got ${quiet.messages.map { it.text }})")
    expect(stopper.messages.map { it.text } == listOf("hello", "psst", "last"), "stopper got all three")
    expect(tail.messages.map { it.text } == listOf("hello", "psst"), "tail got hello,psst (got ${tail.messages.map { it.text }})")

    // Message record fields decoded from the callback's buffer. seq counts
    // from 1 and includes the subscriber-less publish above.
    val first = stopper.messages[0]
    expect(first == Message(2L, "news", "hello", listOf("a", "b")), "first message record (got $first)")
    expect(stopper.messages[1].tags.isEmpty(), "psst carries no tags")
    expect(stopper.messages[2] == Message(4L, "stop", "last", listOf("z")), "last message record (got ${stopper.messages[2]})")

    // Async publish via the suspend wrapper, resumed from the producer thread.
    val later = runBlocking { bus.publishLater("later", "async hello") }
    expect(later == 3L, "publishLater accepted by 3 (got $later)")
    expect(stopper.messages.last().text == "async hello", "async message delivered")
    expect(stopper.messages.last().tags.isEmpty(), "async message has no tags")
    expect(stopper.messages.last().seq == 5L, "async message seq 5 (got ${stopper.messages.last().seq})")

    // Iterator-backed messages(): drained as a Sequence, and again by hand.
    val texts = bus.messages().asSequence().toList()
    expect(
        texts == listOf("nobody home", "hello", "psst", "last", "async hello"),
        "messages in publish order (got $texts)"
    )
    val byHand = mutableListOf<String>()
    val it = bus.messages()
    while (it.hasNext()) byHand.add(it.next())
    expect(byHand == texts, "manual iteration matches")
    expect(thrownBy { it.next() } is NoSuchElementException, "exhausted iterator throws NoSuchElementException")
    // An abandoned iterator can be closed early, and closed twice.
    val partial = bus.messages()
    expect(partial.next() == "nobody home", "partial iterator first element")
    partial.close()
    partial.close()
    expect(!partial.hasNext(), "closed iterator reports no more elements")

    // Nullable record return.
    val last = bus.lastMessage()
    expect(last == Message(5L, "later", "async hello", listOf()), "lastMessage (got $last)")

    // routeOnce: a free function taking the callback interface without a bus.
    expect(Events.routeOnce(quiet, "quiet") == Delivery.Skip, "routeOnce quiet -> Skip")
    expect(Events.routeOnce(stopper, "stop") == Delivery.AcceptAndStop, "routeOnce stop -> AcceptAndStop")
    expect(Events.routeOnce(tail, "anything") == Delivery.Accept, "routeOnce -> Accept")
    expect(quiet.routed.last() == "quiet", "routeOnce reached route()")
    // The producer drops its only reference when routeOnce returns, so the
    // GlobalRef pinning a throwaway subscriber is released and it becomes
    // collectable once the frame that created it is gone.
    expect(collected(routeOnceThrowaway()), "routeOnce released its subscriber (free ran)")

    // The bus handed to onAttached is a distinct wrapper over the same
    // object: usable, and closing it leaves the caller's wrapper alive.
    val kept = tail.keptBus!!
    expect(kept.handle.address == bus.handle.address, "onAttached bus is the same native object")
    expect(kept.subscriberCount() == 3L, "kept bus reads the live subscriber count")
    kept.close()
    kept.close()
    expect(thrownBy { kept.subscriberCount() } is IllegalStateException, "kept bus rejects use after close")
    expect(bus.subscriberCount() == 3L, "original bus still alive after closing the kept wrapper")
    quiet.keptBus!!.close()
    stopper.keptBus!!.close()

    // Foreign errors: a Kotlin exception thrown from any callback method
    // surfaces to the caller as FfiException(-4) carrying the throwable's
    // text, and the JVM keeps running. Use a fresh bus for exact counts.
    EventBus().use { bus2 ->
        val ok = RecordingSubscriber("ok")
        val rejecter = RecordingSubscriber("rejecter", failTopic = "boom")
        bus2.subscribe(ok)
        bus2.subscribe(rejecter)
        ok.keptBus!!.close()
        rejecter.keptBus!!.close()
        val routeErr = thrownBy { bus2.publish("boom", "x", listOf()) }
        expect(routeErr is FfiException, "throwing route() surfaces as FfiException (got $routeErr)")
        expect((routeErr as FfiException).code == -4, "foreign error code -4 (got ${routeErr.code})")
        expect(
            routeErr.message?.contains("rejected boom") == true,
            "foreign error carries the Kotlin message (got ${routeErr.message})"
        )
        expect(routeErr.message?.contains("IllegalStateException") == true, "foreign error names the exception class")
        // The earlier subscriber was still delivered to, the producer logged
        // the message, and the bus remains usable.
        expect(ok.messages.map { it.text } == listOf("x"), "subscriber before the failure still received x")
        expect(bus2.lastMessage()?.text == "x", "producer logged the message despite the failure")
        expect(bus2.publish("fine", "y", listOf()) == 2L, "bus usable after a foreign error")

        val onMsg = RecordingSubscriber("onMsg", failOnMessage = true)
        bus2.subscribe(onMsg)
        onMsg.keptBus!!.close()
        val msgErr = thrownBy { bus2.publish("any", "z", listOf()) }
        expect(msgErr is FfiException && msgErr.code == -4, "throwing onMessage() surfaces as -4 (got $msgErr)")
        expect(msgErr?.message?.contains("failed on message") == true, "onMessage error text (got ${msgErr?.message})")

        // Throwing from onAttached aborts subscribe itself and the subscriber
        // isn't retained.
        val refuser = RecordingSubscriber("refuser", failOnAttached = true)
        val attachErr = thrownBy { bus2.subscribe(refuser) }
        expect(attachErr is FfiException && attachErr.code == -4, "throwing onAttached() surfaces as -4 (got $attachErr)")
        expect(bus2.subscriberCount() == 3L, "refused subscriber not retained (got ${bus2.subscriberCount()})")

        // routeOnce also propagates the foreign error.
        val onceErr = thrownBy { Events.routeOnce(rejecter, "boom") }
        expect(onceErr is FfiException && onceErr.code == -4, "routeOnce propagates -4 (got $onceErr)")
        expect(Events.routeOnce(rejecter, "calm") == Delivery.Accept, "rejecter still works for other topics")
    }

    // clearSubscribers drops every retained subscriber (each free runs), and
    // the bus keeps its log.
    val goneRef = subscribeThrowaway(bus)
    expect(bus.subscriberCount() == 4L, "subscribe gone")
    bus.clearSubscribers()
    expect(bus.subscriberCount() == 0L, "clearSubscribers empties the bus")
    expect(collected(goneRef), "cleared subscriber was released by the producer")
    expect(bus.publish("after", "nobody", listOf()) == 0L, "publish after clear accepts 0")
    expect(bus.lastMessage()?.text == "nobody", "log continues after clear")
    expect(quiet.messages.size == 3, "cleared subscribers receive nothing more")

    // Close semantics: double close is safe, use after close throws.
    bus.close()
    bus.close()
    expect(thrownBy { bus.subscriberCount() } is IllegalStateException, "bus rejects use after close")
    expect(thrownBy { bus.publish("x", "y", listOf()) } is IllegalStateException, "publish rejects use after close")

    println("kotlin/events: OK")
}

fun main() {
    run()
    expectNoLeaks { JniBridge.debug_live(it) }
    println("kotlin/events: no leaks")
}
