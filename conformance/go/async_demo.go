// Conformance consumer: async-demo sample, Go target.
//
// Drives the async bridge end to end: every async wrapper takes a
// context.Context and blocks until the producer's completion callback fires.
// RunTask decodes the TaskResult record from a value buffer, an empty name
// reports the typed *TaskError (matched via errors.As), RunBatch round-trips
// a list of records both ways, and RunNTasks returns a direct scalar.
//
// Cancellation: Wait is cancellable. Left alone it completes after its
// timeout; cancelling its context (or letting a deadline expire) cancels the
// native token, and the call returns promptly with the context's error. An
// already-cancelled context never launches. Exits 0 on success; aborts
// (non-zero) on any mismatch.

package main

import (
	"context"
	"errors"
	"fmt"
	"time"

	wv "__MODPATH__"
)

func main() {
	ctx := context.Background()

	// Async record return.
	result, err := wv.RunTask(ctx, "alpha")
	expect(err == nil, fmt.Sprintf("run_task succeeds (got %v)", err))
	expect(result.Id > 0, "run_task assigns an id")
	expect(result.Value == "completed: alpha", fmt.Sprintf("run_task value (got %q)", result.Value))
	expect(result.Success, "run_task success flag")

	// Typed async error: the empty name reports TaskError InvalidName.
	_, err = wv.RunTask(ctx, "")
	var taskErr *wv.TaskError
	expect(errors.As(err, &taskErr), fmt.Sprintf("typed *TaskError (got %T)", err))
	expect(taskErr.Code == wv.TaskErrorInvalidName, fmt.Sprintf("InvalidName carries code 1 (got %d)", taskErr.Code))

	// Buffered list-of-records both ways.
	batch, err := wv.RunBatch(ctx, []string{"a", "b", "c"})
	expect(err == nil && len(batch) == 3, fmt.Sprintf("run_batch returns 3 results (got %d, %v)", len(batch), err))
	for i, want := range []string{"completed: a", "completed: b", "completed: c"} {
		expect(batch[i].Value == want, fmt.Sprintf("run_batch[%d] value (got %q)", i, batch[i].Value))
		expect(batch[i].Success, "run_batch success flag")
	}

	// Direct scalar through the async callback.
	n, err := wv.RunNTasks(ctx, 7)
	expect(err == nil && n == 7, "run_n_tasks echoes n")

	// A cancellable call left alone completes normally.
	waited, err := wv.Wait(ctx, 20)
	expect(err == nil && waited == 20, fmt.Sprintf("wait(20) completes (got %d, %v)", waited, err))

	// Cancelling the context cancels the native call promptly.
	cctx, cancel := context.WithCancel(ctx)
	time.AfterFunc(50*time.Millisecond, cancel)
	start := time.Now()
	_, err = wv.Wait(cctx, 30_000)
	elapsed := time.Since(start)
	expect(errors.Is(err, context.Canceled), fmt.Sprintf("cancelled wait returns context.Canceled (got %v)", err))
	expect(elapsed < 5*time.Second, fmt.Sprintf("cancellation is prompt (took %v)", elapsed))

	// A deadline cancels the same way and reports DeadlineExceeded.
	dctx, stop := context.WithTimeout(ctx, 50*time.Millisecond)
	_, err = wv.Wait(dctx, 30_000)
	stop()
	expect(errors.Is(err, context.DeadlineExceeded), fmt.Sprintf("deadline yields DeadlineExceeded (got %v)", err))

	// An already-cancelled context never launches the call.
	done, cancelNow := context.WithCancel(ctx)
	cancelNow()
	_, err = wv.Wait(done, 30_000)
	expect(errors.Is(err, context.Canceled), "pre-cancelled wait returns context.Canceled")
	_, err = wv.RunTask(done, "never")
	expect(errors.Is(err, context.Canceled), "pre-cancelled run_task returns context.Canceled")

	// Many concurrent cancellations all settle.
	errs := make(chan error, 16)
	for range 16 {
		go func() {
			c, cancel := context.WithTimeout(ctx, 10*time.Millisecond)
			defer cancel()
			_, err := wv.Wait(c, 30_000)
			errs <- err
		}()
	}
	for range 16 {
		expect(errors.Is(<-errs, context.DeadlineExceeded), "concurrent cancellation")
	}

	// Every task body has finished: cancelled ones were dropped by the
	// runtime before their completion fired.
	deadline := time.Now().Add(5 * time.Second)
	for wv.ActiveCallbacks() != 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	expect(wv.ActiveCallbacks() == 0, "active_callbacks settles to zero")

	expectNoLeaks(wv.DebugLive)
	fmt.Println("go/async-demo: OK")
}
