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

	msgs := wv.GetMessages()
	expect(len(msgs) == 2 && msgs[0] == "alpha" && msgs[1] == "beta",
		fmt.Sprintf("iterator yields messages in order (got %v)", msgs))

	// Unregister stops delivery; the producer still records the message.
	wv.UnregisterMessageListener(sub)
	wv.SendMessage("gamma")
	expect(len(received) == 2, fmt.Sprintf("no delivery after unregister (got %v)", received))

	msgs = wv.GetMessages()
	expect(len(msgs) == 3, "producer kept recording")

	fmt.Println("go/events: OK")
}
