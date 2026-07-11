// Conformance consumer: events sample, Go target.
//
// Exercises the cgo //export listener trampoline (register -> the producer
// fires the Go closure synchronously on send -> unregister stops delivery)
// and the opaque-iterator ABI behind GetMessages. Everything here is
// non-throwing, so the wrappers have plain returns (no error results).
// Exits 0 on success; aborts (non-zero) on any mismatch.

package main

import (
	"fmt"
	"os"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

func main() {
	var received []string
	sub := wv.RegisterMessageListener(func(message string) {
		received = append(received, message)
	})
	expect(sub > 0, "listener id positive")

	wv.SendMessage("alpha")
	wv.SendMessage("beta")
	expect(len(received) == 2 && received[0] == "alpha" && received[1] == "beta",
		fmt.Sprintf("listener received sends (got %v)", received))

	// GetMessages returns a lazy iter.Seq[string]: one producer next per step.
	var msgs []string
	for m := range wv.GetMessages() {
		msgs = append(msgs, m)
	}
	expect(len(msgs) == 2 && msgs[0] == "alpha" && msgs[1] == "beta",
		fmt.Sprintf("iterator yields messages in order (got %v)", msgs))

	// Unregister stops delivery; the producer still records the message.
	wv.UnregisterMessageListener(sub)
	wv.SendMessage("gamma")
	expect(len(received) == 2, fmt.Sprintf("no delivery after unregister (got %v)", received))

	msgs = msgs[:0]
	for m := range wv.GetMessages() {
		msgs = append(msgs, m)
	}
	expect(len(msgs) == 3, "producer kept recording")

	// Early break destroys the iterator handle without draining it.
	first := ""
	for m := range wv.GetMessages() {
		first = m
		break
	}
	expect(first == "alpha", "early break yields only the first message")

	fmt.Println("go/events: OK")
}
