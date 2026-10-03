// Shared helpers for the Go conformance consumers. run.sh copies this file
// next to each consumer's main.go.

package main

import (
	"fmt"
	"os"
	"runtime"
	"time"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

// catchPanic runs f and returns the recovered panic value (nil if none).
func catchPanic(f func()) (v any) {
	defer func() { v = recover() }()
	f()
	return nil
}

// expectNoLeaks drives the garbage collector until every finalizer has run
// and then asserts that the producer holds no live objects, callbacks,
// iterators, cancel tokens, or returned allocations.
func expectNoLeaks(debugLive func(int32) uint64) {
	kinds := []string{"objects", "callbacks", "iterators", "cancel tokens", "allocations"}
	deadline := time.Now().Add(10 * time.Second)
	for {
		runtime.GC()
		leaked := ""
		for k, name := range kinds {
			if n := debugLive(int32(k)); n != 0 {
				leaked += fmt.Sprintf(" %s=%d", name, n)
			}
		}
		if leaked == "" {
			return
		}
		expect(time.Now().Before(deadline), "live native resources at exit:"+leaked)
		time.Sleep(10 * time.Millisecond)
	}
}
