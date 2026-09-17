// Conformance consumer: events sample, Go target.
//
// Exercises the ABI 2 surface end to end: a Go type implementing the
// generated Subscriber callback interface (crossing as a cgo.Handle plus a
// static vtable of //export trampolines), the reference-counted EventBus
// object (including a second wrapper handed back through OnAttached, its
// explicit Close, and a harmless double Close), Delivery return values
// steering Publish's accepted count, the Message record decoded from a
// borrowed value buffer inside a callback, the async PublishLater bridge,
// the lazy iter.Seq behind Messages, the optional LastMessage, the free
// function RouteOnce, and the foreign-error path: a subscriber that panics
// surfaces to the caller as a recoverable *WeaveFFIError with code -4 and
// leaves the bus usable. Finally ClearSubscribers must let the producer's
// `free` entry run, which is observed through a Go finalizer on the
// implementation once its handle is deleted. Exits 0 on success; aborts
// (non-zero) on any mismatch.

package main

import (
	"errors"
	"fmt"
	"os"
	"runtime"
	"strings"
	"sync/atomic"
	"time"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

// subState is the observable side of a subscriber. It's held separately
// from the implementing value so the implementation itself can become
// unreachable (and be finalized) once the producer frees its handle.
type subState struct {
	name     string
	skip     string
	fails    bool
	routed   []string
	received []wv.Message
	attached int
	bus      *wv.EventBus
	freed    *atomic.Int32
}

// recorder implements wv.Subscriber. The subscriber's configured skip topic
// is skipped and "stop" accepts and stops later subscribers. A subscriber
// marked `fails` panics inside Route for "boom-route" and inside OnMessage
// for "boom-message"; the others accept both, so the earlier subscribers
// still receive them.
type recorder struct {
	st *subState
}

func (r *recorder) Route(topic string) wv.Delivery {
	r.st.routed = append(r.st.routed, topic)
	switch topic {
	case r.st.skip:
		return wv.DeliverySkip
	case "stop":
		return wv.DeliveryAcceptAndStop
	case "boom-route":
		if r.st.fails {
			panic("subscriber " + r.st.name + " rejected topic")
		}
	}
	return wv.DeliveryAccept
}

func (r *recorder) OnMessage(message wv.Message) int64 {
	if message.Topic == "boom-message" && r.st.fails {
		panic(fmt.Errorf("subscriber %s failed on message %d", r.st.name, message.Seq))
	}
	r.st.received = append(r.st.received, message)
	return int64(len(r.st.received))
}

func (r *recorder) OnAttached(bus *wv.EventBus) {
	r.st.attached++
	// The reference is ours; keep the wrapper so main can prove it aliases
	// the bus that called subscribe.
	r.st.bus = bus
}

// attach subscribes a fresh recorder and returns only its state. The
// recorder value stays reachable solely through the cgo.Handle the
// producer holds, so its finalizer fires once the producer calls `free`.
func attach(bus *wv.EventBus, name, skip string, fails bool, freed *atomic.Int32) (*subState, int64) {
	st := &subState{name: name, skip: skip, fails: fails, freed: freed}
	r := &recorder{st: st}
	runtime.SetFinalizer(r, func(*recorder) { freed.Add(1) })
	return st, bus.Subscribe(r)
}

// catchPanic runs f and returns the recovered panic value (nil if none).
func catchPanic(f func()) (v any) {
	defer func() { v = recover() }()
	f()
	return nil
}

// expectForeignError asserts that v is the generated brand error carrying
// FOREIGN_ERROR_CODE (-4) and the Go panic text.
func expectForeignError(v any, needle string, what string) {
	expect(v != nil, what+": expected a panic")
	err, isErr := v.(error)
	expect(isErr, fmt.Sprintf("%s: panic value is an error (got %T)", what, v))
	var ferr *wv.WeaveFFIError
	expect(errors.As(err, &ferr), fmt.Sprintf("%s: *WeaveFFIError (got %T: %v)", what, v, v))
	expect(ferr.Code == -4, fmt.Sprintf("%s: code -4 (got %d)", what, ferr.Code))
	expect(strings.Contains(ferr.Message, needle),
		fmt.Sprintf("%s: message carries the Go panic text (got %q)", what, ferr.Message))
}

// waitFreed drives the GC until n implementations have been finalized.
func waitFreed(freed *atomic.Int32, n int32) bool {
	for i := 0; i < 200; i++ {
		runtime.GC()
		if freed.Load() >= n {
			return true
		}
		time.Sleep(5 * time.Millisecond)
	}
	return freed.Load() >= n
}

func main() {
	var freed atomic.Int32
	bus := wv.NewEventBus()
	expect(bus.SubscriberCount() == 0, "fresh bus has no subscribers")
	expect(bus.LastMessage() == nil, "fresh bus has no last message")

	// Subscribe: the producer calls OnAttached with a strong reference to
	// the bus before retaining the subscriber, then returns the new count.
	a, n := attach(bus, "a", "quiet", false, &freed)
	expect(n == 1, fmt.Sprintf("first subscribe returns 1 (got %d)", n))
	expect(a.attached == 1, "OnAttached called exactly once")
	expect(a.bus != nil, "OnAttached received a bus wrapper")
	expect(a.bus.SubscriberCount() == 1, "the attached wrapper aliases the live bus")

	b, n := attach(bus, "b", "", false, &freed)
	expect(n == 2, fmt.Sprintf("second subscribe returns 2 (got %d)", n))
	expect(bus.SubscriberCount() == 2, "subscriber_count == 2")

	// Delivery steers the accepted count: both accept "news".
	delivered := bus.Publish("news", "hello", []string{"x", "yy"})
	expect(delivered == 2, fmt.Sprintf("news accepted by both (got %d)", delivered))
	expect(len(a.received) == 1 && len(b.received) == 1, "both subscribers received news")
	m := a.received[0]
	expect(m.Seq == 1, fmt.Sprintf("seq starts at 1 (got %d)", m.Seq))
	expect(m.Topic == "news" && m.Text == "hello", "message topic/text")
	expect(len(m.Tags) == 2 && m.Tags[0] == "x" && m.Tags[1] == "yy",
		fmt.Sprintf("tags round-trip through the borrowed buffer (got %v)", m.Tags))
	expect(a.routed[0] == "news" && b.routed[0] == "news", "route called with the topic")

	// "quiet": a skips, b accepts.
	delivered = bus.Publish("quiet", "psst", nil)
	expect(delivered == 1, fmt.Sprintf("quiet accepted by one (got %d)", delivered))
	expect(len(a.received) == 1, "a skipped quiet")
	expect(len(b.received) == 2 && b.received[1].Seq == 2 && len(b.received[1].Tags) == 0,
		"b received quiet with seq 2 and empty tags")

	// "stop": a accepts and stops, so b is never routed.
	delivered = bus.Publish("stop", "last", []string{})
	expect(delivered == 1, fmt.Sprintf("stop accepted by one (got %d)", delivered))
	expect(len(a.received) == 2 && a.received[1].Text == "last", "a received stop")
	expect(len(b.routed) == 2, fmt.Sprintf("b not routed after AcceptAndStop (got %v)", b.routed))

	// The wrapper handed through OnAttached sees the same log.
	last := a.bus.LastMessage()
	expect(last != nil && last.Seq == 3 && last.Topic == "stop" && last.Text == "last",
		fmt.Sprintf("last_message via the attached wrapper (got %+v)", last))

	// Async: the producer publishes from its own thread, and the callback
	// trampolines run there; the wrapper parks the goroutine until done.
	delivered = bus.PublishLater("async", "later")
	expect(delivered == 2, fmt.Sprintf("publish_later accepted by both (got %d)", delivered))
	expect(len(a.received) == 3 && a.received[2].Seq == 4 && a.received[2].Topic == "async",
		"a received the async message")
	expect(len(b.received) == 3 && b.received[2].Text == "later", "b received the async message")

	// Iterator: lazy iter.Seq over every message text, in order.
	var texts []string
	for t := range bus.Messages() {
		texts = append(texts, t)
	}
	expect(len(texts) == 4 && texts[0] == "hello" && texts[1] == "psst" &&
		texts[2] == "last" && texts[3] == "later",
		fmt.Sprintf("messages iterator in order (got %v)", texts))
	first := ""
	for t := range bus.Messages() {
		first = t
		break
	}
	expect(first == "hello", "early break yields only the first message")

	last = bus.LastMessage()
	expect(last != nil && last.Seq == 4 && last.Text == "later" && len(last.Tags) == 0,
		fmt.Sprintf("last_message optional present (got %+v)", last))

	// Free function taking the callback interface: the producer frees the
	// handle right after the call.
	probe := &recorder{st: &subState{name: "probe", skip: "quiet", fails: true}}
	expect(wv.RouteOnce(probe, "quiet") == wv.DeliverySkip, "route_once quiet -> Skip")
	expect(wv.RouteOnce(probe, "stop") == wv.DeliveryAcceptAndStop, "route_once stop -> AcceptAndStop")
	expect(wv.RouteOnce(probe, "other") == wv.DeliveryAccept, "route_once other -> Accept")
	expect(len(probe.st.routed) == 3, "route_once called Route each time")
	expectForeignError(catchPanic(func() { wv.RouteOnce(probe, "boom-route") }),
		"subscriber probe rejected topic", "route_once foreign error")

	// Foreign errors through the bus: a Go panic inside Route or OnMessage
	// aborts publish with code -4, and the bus stays usable afterwards.
	f, n := attach(bus, "f", "", true, &freed)
	expect(n == 3 && f.attached == 1, "third subscriber attached")
	expectForeignError(catchPanic(func() { bus.Publish("boom-route", "x", nil) }),
		"subscriber f rejected topic", "publish with panicking Route")
	expectForeignError(catchPanic(func() { bus.Publish("boom-message", "y", nil) }),
		"subscriber f failed on message", "publish with panicking OnMessage")
	// The bus logged both aborted publishes and still delivers.
	last = bus.LastMessage()
	expect(last != nil && last.Seq == 6 && last.Topic == "boom-message", "aborted publishes were logged")
	delivered = bus.Publish("ok", "z", nil)
	expect(delivered == 3, fmt.Sprintf("bus usable after foreign error (got %d)", delivered))
	expect(len(f.received) == 1 && f.received[0].Seq == 7, "f received the follow-up message")
	expect(bus.SubscriberCount() == 3, "subscriber_count == 3")

	// Releasing subscribers runs each `free`, which deletes the cgo.Handle
	// and lets the Go implementation be collected.
	bus.ClearSubscribers()
	expect(bus.SubscriberCount() == 0, "subscriber_count == 0 after clear")
	expect(waitFreed(&freed, 3), fmt.Sprintf("all three subscribers freed (got %d)", freed.Load()))
	delivered = bus.Publish("after", "nobody", nil)
	expect(delivered == 0, "no subscribers accept after clear")

	// Every wrapper releases its own reference; Close is idempotent.
	a.bus.Close()
	a.bus.Close()
	expect(bus.SubscriberCount() == 0, "bus outlives the attached wrapper's release")
	b.bus.Close()
	f.bus.Close()
	bus.Close()
	bus.Close()
	fmt.Println("go/events: OK")
}
