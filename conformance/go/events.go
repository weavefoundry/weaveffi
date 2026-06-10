// Conformance consumer: events sample, Go target.
//
// Exercises the cgo //export listener trampoline (register -> the producer
// fires the Go closure synchronously on send -> unregister stops delivery)
// and the opaque-iterator ABI behind EventsGetMessages. Exits 0 on success;
// aborts (non-zero) on any mismatch.

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
	sub := wv.EventsRegisterMessageListener(func(message string) {
		received = append(received, message)
	})
	expect(sub > 0, "listener id positive")

	expect(wv.EventsSendMessage("alpha") == nil, "send alpha")
	expect(wv.EventsSendMessage("beta") == nil, "send beta")
	expect(len(received) == 2 && received[0] == "alpha" && received[1] == "beta",
		fmt.Sprintf("listener received sends (got %v)", received))

	msgs, err := wv.EventsGetMessages()
	expect(err == nil, "get messages")
	expect(len(msgs) == 2 && msgs[0] == "alpha" && msgs[1] == "beta",
		fmt.Sprintf("iterator yields messages in order (got %v)", msgs))

	// Unregister stops delivery; the producer still records the message.
	wv.EventsUnregisterMessageListener(sub)
	expect(wv.EventsSendMessage("gamma") == nil, "send gamma")
	expect(len(received) == 2, fmt.Sprintf("no delivery after unregister (got %v)", received))

	msgs, err = wv.EventsGetMessages()
	expect(err == nil && len(msgs) == 3, "producer kept recording")

	fmt.Println("go/events: OK")
}
