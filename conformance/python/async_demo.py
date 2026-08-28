"""Conformance consumer: async-demo sample, Python target.

Drives the asyncio-bridged async surface end to end: `run_task` as a
coroutine settled from the producer's worker thread and decoded from a value
buffer into the `TaskResult` dataclass, the typed `TaskError.InvalidName`
raised by a throwing coroutine, the buffered list-of-records round trip
through `run_batch`, the direct-scalar `run_n_tasks`, the sync `cancel_task`,
and `active_callbacks` returning to zero once every task body has completed.
The generated package is placed on sys.path via WV_PY; the cdylib is selected
with WEAVEFFI_LIBRARY.
"""
import asyncio
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import async_demo as wv  # noqa: E402


def main() -> None:
    # Async record return: the coroutine resolves with a TaskResult decoded
    # from the completion callback's value buffer.
    result = asyncio.run(wv.run_task("alpha"))
    assert isinstance(result, wv.TaskResult), type(result)
    assert result.id > 0, result.id
    assert result.value == "completed: alpha", result.value
    assert result.success is True

    # Typed async error: an empty name settles the coroutine with the
    # InvalidName subclass carrying its stable code.
    try:
        asyncio.run(wv.run_task(""))
        raise AssertionError("expected TaskError.InvalidName for empty name")
    except wv.TaskError.InvalidName as exc:
        assert exc.code == 1, exc.code
        assert isinstance(exc, wv.TaskError)
        assert isinstance(exc, wv.WeaveFFIError)

    # Buffered list-of-records both ways: the names list crosses in one value
    # buffer, the results come back in another.
    batch = asyncio.run(wv.run_batch(["a", "b", "c"]))
    assert [r.value for r in batch] == [
        "completed: a",
        "completed: b",
        "completed: c",
    ], batch
    assert all(r.success for r in batch)

    # Direct scalar through the async callback.
    assert asyncio.run(wv.run_n_tasks(7)) == 7

    # Sync functions beside the async ones.
    assert wv.cancel_task(1) is False

    # Every spawned task body has completed by the time its callback fires.
    assert wv.active_callbacks() == 0

    print("python async-demo conformance: OK")


if __name__ == "__main__":
    main()
