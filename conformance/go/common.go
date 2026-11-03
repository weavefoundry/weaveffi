// Shared helpers for the Go conformance consumers. run.sh copies this file
// next to each consumer's main.go.

package main

import (
	"errors"
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

// expectAs asserts that err matches the error type T and returns the match.
func expectAs[T error](err error, what string) T {
	var target T
	expect(errors.As(err, &target), fmt.Sprintf("%s: want %T, got %T (%v)", what, target, err, err))
	return target
}

// expectNoLeaks checks that the library counts live resources, then drives
// the garbage collector until every finalizer has run and asserts that the
// library holds no live objects, callbacks, iterators, cancel tokens, or byte
// runs. Async workers may still be dropping a finished call, so the counters
// get a few seconds to settle.
func expectNoLeaks(debugLive func(int32) uint64) {
	expect(debugLive(-1) == 1, "the library counts live resources")
	kinds := []string{"objects", "callbacks", "iterators", "cancel tokens", "byte runs"}
	deadline := time.Now().Add(5 * time.Second)
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
