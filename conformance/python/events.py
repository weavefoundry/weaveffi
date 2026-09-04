"""Conformance consumer: events sample, Python target.

Exercises the ABI 2 events surface through the generated ctypes wrapper: the
`Subscriber` callback interface implemented by subclassing the generated ABC
(every method is called with the right arguments, the `EventBus` handed to
`on_attached` is a usable wrapper that owns its own strong reference), the
`Delivery` return steering `publish`'s accepted count, the reference-counted
`EventBus` object (`with` statement, idempotent `close()`, use after close
rejected), the asyncio-bridged `publish_later` method, the `messages()`
iterator, the `last_message()` optional record, `route_once` releasing a
subscriber it doesn't retain, and a subscriber that raises a Python exception
surfacing to the caller as `WeaveFFIError` with `FOREIGN_ERROR_CODE` (-4)
without taking the interpreter down. The generated package is placed on
sys.path via WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import asyncio
import gc
import os
import sys
import weakref

sys.path.insert(0, os.environ["WV_PY"])

import events as wv  # noqa: E402


def check(cond: bool, what: str) -> None:
    if not cond:
        print(f"python/events: FAIL: {what}", file=sys.stderr)
        sys.exit(1)


class Recorder(wv.Subscriber):
    """A subscriber that records every callback it receives.

    `skip_topic` is routed Skip, `stop_topic` AcceptAndStop, `fail_topic`
    raises from `route`, `fail_text` raises from `on_message`, and
    everything else is Accept. The bus received in `on_attached` is kept so
    the consumer can prove it stays alive independently of the caller's
    wrapper.
    """

    def __init__(self, name: str, skip_topic: str = "", stop_topic: str = "",
                 fail_topic: str = "", fail_text: str = "") -> None:
        self.name = name
        self.skip_topic = skip_topic
        self.stop_topic = stop_topic
        self.fail_topic = fail_topic
        self.fail_text = fail_text
        self.routed: list[str] = []
        self.messages: list[wv.Message] = []
        self.attached: list[wv.EventBus] = []
        self.attach_counts: list[int] = []

    def route(self, topic: str) -> wv.Delivery:
        self.routed.append(topic)
        if topic == self.fail_topic:
            raise ValueError(f"{self.name} rejected topic {topic}")
        if topic == self.skip_topic:
            return wv.Delivery.Skip
        if topic == self.stop_topic:
            return wv.Delivery.AcceptAndStop
        return wv.Delivery.Accept

    def on_message(self, message: wv.Message) -> int:
        if message.text == self.fail_text:
            raise RuntimeError(f"{self.name} choked on {message.text}")
        self.messages.append(message)
        return len(self.messages)

    def on_attached(self, bus: wv.EventBus) -> None:
        # The wrapper is usable right away: subscribe has not pushed us yet.
        self.attach_counts.append(bus.subscriber_count())
        self.attached.append(bus)


class Exploding(wv.Subscriber):
    """A subscriber whose `on_attached` raises, so `subscribe` itself fails."""

    def route(self, topic: str) -> wv.Delivery:
        return wv.Delivery.Accept

    def on_message(self, message: wv.Message) -> int:
        return 0

    def on_attached(self, bus: wv.EventBus) -> None:
        bus.close()
        raise KeyError("refusing to attach")


def main() -> None:
    check(wv.Delivery.Accept == 0 and wv.Delivery.Skip == 1
          and wv.Delivery.AcceptAndStop == 2, "Delivery discriminants")

    with wv.EventBus() as bus:
        check(bus.subscriber_count() == 0, "fresh bus has no subscribers")
        check(bus.last_message() is None, "fresh bus has no last message")
        check(list(bus.messages()) == [], "fresh bus has no messages")

        quiet = Recorder("quiet", skip_topic="quiet")
        loud = Recorder("loud", stop_topic="stop")
        tail = Recorder("tail")

        check(bus.subscribe(quiet) == 1, "first subscribe returns 1")
        check(bus.subscribe(loud) == 2, "second subscribe returns 2")
        check(bus.subscribe(tail) == 3, "third subscribe returns 3")
        check(bus.subscriber_count() == 3, "subscriber_count after three subscribes")

        # on_attached fired once per subscriber, before the push, with a
        # live bus wrapper.
        check(quiet.attach_counts == [0], f"quiet attach count {quiet.attach_counts}")
        check(loud.attach_counts == [1], f"loud attach count {loud.attach_counts}")
        check(tail.attach_counts == [2], f"tail attach count {tail.attach_counts}")
        check(len(quiet.attached) == 1, "quiet kept its attached bus")
        check(isinstance(quiet.attached[0], wv.EventBus),
              "on_attached receives an EventBus wrapper")
        # The adopted reference is independent of the caller's wrapper: it is
        # a different Python object on the same underlying bus.
        check(quiet.attached[0] is not bus, "attached wrapper is a distinct object")
        check(quiet.attached[0].subscriber_count() == 3,
              "attached bus wrapper observes the shared state")

        # Accept everywhere: every subscriber gets it.
        n = bus.publish("news", "hello", ["a", "b"])
        check(n == 3, f"publish news accepted {n}")
        check(quiet.routed == ["news"] and loud.routed == ["news"]
              and tail.routed == ["news"], "route called on each subscriber")
        for sub in (quiet, loud, tail):
            check(len(sub.messages) == 1, f"{sub.name} received one message")
            m = sub.messages[0]
            check(m == wv.Message(seq=1, topic="news", text="hello", tags=["a", "b"]),
                  f"{sub.name} message {m}")

        # Skip steers the count: quiet drops its own topic.
        n = bus.publish("quiet", "psst", [])
        check(n == 2, f"publish quiet accepted {n}")
        check(len(quiet.messages) == 1, "quiet skipped its topic")
        check(loud.messages[-1].seq == 2 and loud.messages[-1].tags == [],
              "seq advances and empty tags round-trip")

        # AcceptAndStop: loud accepts and stops delivery to tail.
        n = bus.publish("stop", "last", ["z"])
        check(n == 2, f"publish stop accepted {n}")
        check(len(quiet.messages) == 2, "quiet accepted stop")
        check(len(loud.messages) == 3, "loud accepted stop")
        check(len(tail.messages) == 2, "tail was not reached after AcceptAndStop")
        check(tail.routed == ["news", "quiet"], f"tail routed {tail.routed}")

        # Async publish: resolved from the producer's thread through asyncio.
        n = asyncio.run(bus.publish_later("later", "async"))
        check(n == 3, f"publish_later accepted {n}")
        check(tail.messages[-1] == wv.Message(4, "later", "async", []),
              f"async message {tail.messages[-1]}")

        # Iterator and optional record.
        check(list(bus.messages()) == ["hello", "psst", "last", "async"],
              "messages() streams every text in order")
        it = bus.messages()
        check(next(it) == "hello", "manual next on iterator")
        it.close()
        it.close()
        last = bus.last_message()
        check(last == wv.Message(seq=4, topic="later", text="async", tags=[]),
              f"last_message {last}")

        # A subscriber that raises from route aborts publish with the foreign
        # error code, and the bus is still usable afterwards.
        grumpy = Recorder("grumpy", fail_topic="boom", fail_text="gag")
        check(bus.subscribe(grumpy) == 4, "grumpy subscribed")
        try:
            bus.publish("boom", "x", [])
            check(False, "expected WeaveFFIError from a raising route")
        except wv.WeaveFFIError as exc:
            check(exc.code == wv.WeaveFFIError.FOREIGN_ERROR_CODE == -4,
                  f"foreign error code {exc.code}")
            check("grumpy rejected topic boom" in exc.message,
                  f"foreign error message {exc.message!r}")
        # Earlier subscribers were already delivered before grumpy raised.
        check(len(quiet.messages) == 4, "quiet got the aborted message first")
        # A raise from on_message is reported the same way.
        try:
            bus.publish("ok", "gag", [])
            check(False, "expected WeaveFFIError from a raising on_message")
        except wv.WeaveFFIError as exc:
            check(exc.code == -4, f"on_message foreign code {exc.code}")
            check("choked on gag" in exc.message, f"on_message message {exc.message!r}")
        # The log still recorded both aborted publishes.
        check(bus.last_message().text == "gag" and bus.last_message().seq == 6,
              "aborted publishes are still logged")
        n = bus.publish("ok", "recovered", [])
        check(n == 4, f"bus usable after foreign error, accepted {n}")
        check(grumpy.messages[-1].text == "recovered", "grumpy delivered after recovering")

        # A raise from on_attached fails subscribe itself and leaves the
        # subscriber unretained (its free runs during the abort).
        exploding = Exploding()
        exploding_ref = weakref.ref(exploding)
        try:
            bus.subscribe(exploding)
            check(False, "expected WeaveFFIError from a raising on_attached")
        except wv.WeaveFFIError as exc:
            check(exc.code == -4, f"on_attached foreign code {exc.code}")
            check("refusing to attach" in exc.message, f"on_attached message {exc.message!r}")
        check(bus.subscriber_count() == 4, "failed subscribe did not retain")
        del exploding
        gc.collect()
        check(exploding_ref() is None, "unretained subscriber released after failed subscribe")

        # route_once does not retain the subscriber: its free runs before the
        # call returns, so nothing but our local keeps it alive.
        probe = Recorder("probe", skip_topic="quiet")
        probe_ref = weakref.ref(probe)
        check(wv.route_once(probe, "quiet") == wv.Delivery.Skip, "route_once Skip")
        check(wv.route_once(probe, "other") == wv.Delivery.Accept, "route_once Accept")
        check(probe.routed == ["quiet", "other"], f"route_once routed {probe.routed}")
        check(probe.attached == [], "route_once never attaches")
        del probe
        gc.collect()
        check(probe_ref() is None, "route_once released its subscriber")
        try:
            wv.route_once(Recorder("x", fail_topic="bad"), "bad")
            check(False, "expected WeaveFFIError from route_once")
        except wv.WeaveFFIError as exc:
            check(exc.code == -4, f"route_once foreign code {exc.code}")

        # Clearing the subscribers runs each free: once the consumer drops its
        # own references, nothing keeps the implementations alive.
        refs = [weakref.ref(s) for s in (quiet, loud, tail, grumpy)]
        kept_bus = quiet.attached[0]
        del quiet, loud, tail, grumpy, sub
        gc.collect()
        check(all(r() is not None for r in refs), "retained subscribers stay alive")
        bus.clear_subscribers()
        check(bus.subscriber_count() == 0, "clear_subscribers empties the bus")
        gc.collect()
        check(all(r() is None for r in refs), "cleared subscribers were freed")

        # The bus reference adopted through on_attached outlives the caller's
        # wrapper: closing ours leaves the object alive through the other.
        bus.close()
        bus.close()
        try:
            bus.subscriber_count()
            check(False, "expected use-after-close error")
        except wv.WeaveFFIError as exc:
            check("after close" in exc.message, f"use-after-close message {exc.message!r}")
        check(list(kept_bus.messages()) == ["hello", "psst", "last", "async", "x", "gag", "recovered"],
              "bus alive through the callback-adopted reference")
        kept_bus.close()
        kept_bus.close()
    # Leaving the with block closes an already-closed bus: a no-op.

    # A fresh bus is independent state.
    other = wv.EventBus()
    check(other.subscriber_count() == 0 and other.last_message() is None,
          "second bus is independent")
    other.close()

    print("python/events: OK")


main()
