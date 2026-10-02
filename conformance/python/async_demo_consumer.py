"""Conformance consumer: async-demo sample, Python target.

Drives the asyncio-bridged async surface end to end: `run_task` as a
coroutine settled from the producer's worker thread and decoded from a value
buffer into the `TaskResult` dataclass, the typed `TaskError.InvalidName`
raised by a throwing coroutine, the buffered list-of-records round trip
through `run_batch`, the direct-scalar `run_n_tasks` (many at once), and
real cancellation: cancelling the task awaiting the cancellable `wait`
cancels the native token, so the call ends promptly with CancelledError
instead of waiting out its timeout, while an uncancelled `wait` completes
with its value. Ends with `active_callbacks` back at zero and the leak check
(see harness.py).
"""
import asyncio
import time

import async_demo as wv
from harness import Consumer

consumer = Consumer("async-demo")
check = consumer.check


async def basics() -> None:
    result = await wv.run_task("alpha")
    check(isinstance(result, wv.TaskResult), f"run_task type {type(result)}")
    check(result.id > 0 and result.value == "completed: alpha" and result.success is True,
          f"run_task {result}")

    try:
        await wv.run_task("")
        check(False, "expected TaskError.InvalidName for an empty name")
    except wv.TaskError.InvalidName as exc:
        check(exc.code == 1 and isinstance(exc, wv.TaskError) and isinstance(exc, wv.Error),
              f"InvalidName {exc!r}")
        check(exc.message == "task name must not be empty", f"message {exc.message!r}")

    batch = await wv.run_batch(["a", "b", "c"])
    check([r.value for r in batch] == ["completed: a", "completed: b", "completed: c"],
          f"run_batch {batch}")
    check(await wv.run_batch([]) == [], "empty run_batch")

    results = await asyncio.gather(*(wv.run_n_tasks(i) for i in range(32)))
    check(results == list(range(32)), f"run_n_tasks gather {results}")


async def cancellation() -> None:
    # An uncancelled wait completes with the milliseconds waited.
    check(await wv.wait(10) == 10, "wait(10)")

    # Cancelling the awaiting task cancels the native call: it ends right
    # away with CancelledError instead of after its one-minute timeout.
    start = time.monotonic()
    task = asyncio.ensure_future(wv.wait(60_000))
    await asyncio.sleep(0.05)
    task.cancel()
    try:
        await task
        check(False, "expected CancelledError")
    except asyncio.CancelledError:
        pass
    check(task.cancelled(), "task reports cancelled")
    check(time.monotonic() - start < 5, "cancellation did not wait for the timeout")

    # A timeout is a cancellation too.
    start = time.monotonic()
    try:
        await asyncio.wait_for(wv.wait(60_000), 0.05)
        check(False, "expected TimeoutError")
    except asyncio.TimeoutError:
        pass
    check(time.monotonic() - start < 5, "wait_for did not wait for the native timeout")

    # Cancelling before the call ever runs is fine as well.
    task = asyncio.ensure_future(wv.wait(60_000))
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        pass
    check(task.cancelled(), "immediately cancelled task")

    # Many concurrent cancellable calls, half of them cancelled.
    tasks = [asyncio.ensure_future(wv.wait(20 if i % 2 else 60_000)) for i in range(16)]
    await asyncio.sleep(0.05)
    for i, t in enumerate(tasks):
        if not i % 2:
            t.cancel()
    outcomes = await asyncio.gather(*tasks, return_exceptions=True)
    for i, outcome in enumerate(outcomes):
        if i % 2:
            check(outcome == 20, f"wait {i} completed with {outcome!r}")
        else:
            check(isinstance(outcome, asyncio.CancelledError), f"wait {i} cancelled: {outcome!r}")


def main() -> None:
    asyncio.run(basics())
    asyncio.run(cancellation())
    # Every task body has finished (or been dropped) by the time its
    # completion fires.
    for _ in range(50):
        if wv.active_callbacks() == 0:
            break
        time.sleep(0.02)
    check(wv.active_callbacks() == 0, "active_callbacks back at zero")
    consumer.finish(wv)


main()
