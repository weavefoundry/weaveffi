// Conformance consumer: async-demo sample, Go target.
//
// Drives the blocking async bridge end to end: RunTask parks the calling
// goroutine on a channel until the producer's completion callback fires and
// decodes the TaskResult struct from a value buffer, an empty name reports
// the typed *TaskError (code 1, matched via errors.As), RunBatch round-trips
// a list of records through value buffers both ways, RunNTasks returns a
// direct scalar through the callback, CancelTask is a plain sync call, and
// ActiveCallbacks is back to zero once every task body has completed. Exits 0
// on success; aborts (non-zero) on any mismatch.

package main

import (
	"errors"
	"fmt"
	"os"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed: "+msg)
		os.Exit(1)
	}
}

func main() {
	// Async record return: blocks until the worker-thread callback delivers
	// the encoded TaskResult.
	result, err := wv.RunTask("alpha")
	expect(err == nil, "run_task succeeds")
	expect(result.Id > 0, "run_task assigns an id")
	expect(result.Value == "completed: alpha", fmt.Sprintf("run_task value (got %q)", result.Value))
	expect(result.Success, "run_task success flag")

	// Typed async error: the empty name reports TaskError InvalidName.
	_, err = wv.RunTask("")
	expect(err != nil, "empty name reports an error")
	var taskErr *wv.TaskError
	expect(errors.As(err, &taskErr), fmt.Sprintf("typed *TaskError (got %T)", err))
	expect(taskErr.Code == wv.TaskErrorInvalidName, fmt.Sprintf("InvalidName carries code 1 (got %d)", taskErr.Code))

	// Buffered list-of-records both ways.
	batch := wv.RunBatch([]string{"a", "b", "c"})
	expect(len(batch) == 3, fmt.Sprintf("run_batch returns 3 results (got %d)", len(batch)))
	for i, want := range []string{"completed: a", "completed: b", "completed: c"} {
		expect(batch[i].Value == want, fmt.Sprintf("run_batch[%d] value (got %q)", i, batch[i].Value))
		expect(batch[i].Success, "run_batch success flag")
	}

	// Direct scalar through the async callback.
	expect(wv.RunNTasks(7) == 7, "run_n_tasks echoes n")

	// Sync functions beside the async ones.
	expect(!wv.CancelTask(1), "cancel_task reports not cancelled")

	// Every spawned task body has completed by the time its callback fires.
	expect(wv.ActiveCallbacks() == 0, "active_callbacks settles to zero")

	fmt.Println("go async-demo conformance: OK")
}
