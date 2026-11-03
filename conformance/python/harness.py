"""Shared helpers for the Python conformance consumers.

Every consumer imports the generated package as-is (conformance/python/run.sh
puts it on PYTHONPATH and selects the producer library through the loader's
`{PREFIX}_LIBRARY` override). This module adds the assertion helpers, a hook
that turns any exception Python could only report as "unraisable" (raised in
a ctypes callback or a finalizer) into a failure, and the end-of-run leak
check against the producer's live-resource counters.
"""
import gc
import sys
import time
import types
from typing import Any, Callable, List, Type, TypeVar

_unraisable: List[str] = []

E = TypeVar("E", bound=BaseException)


def _record_unraisable(info: Any) -> None:
    _unraisable.append(f"{info.exc_type.__name__}: {info.exc_value} ({info.object!r})")


sys.unraisablehook = _record_unraisable


class Consumer:
    """Assertions for one consumer, labelled `python/<name>`."""

    def __init__(self, name: str, package: types.ModuleType) -> None:
        self.name = name
        self.impl = sys.modules[f"{package.__name__}.{package.__name__}"]
        self.check(self.impl._debug_live(-1) == 1, "the producer counts live resources")

    def check(self, cond: bool, what: str) -> None:
        if not cond:
            print(f"python/{self.name}: FAIL: {what}", file=sys.stderr)
            sys.exit(1)

    def raises(self, cls: Type[E], fn: Callable[[], Any], what: str) -> E:
        """Call `fn`, require it to raise `cls`, and return the exception."""
        try:
            fn()
        except cls as exc:
            return exc
        except BaseException as exc:  # noqa: BLE001
            self.check(False, f"{what}: expected {cls.__name__}, got {exc!r}")
        self.check(False, f"{what}: expected {cls.__name__}, nothing raised")
        raise AssertionError  # unreachable

    def finish(self) -> None:
        """Collect garbage (running every finalizer), then require that the
        producer holds no live object, callback, iterator, cancel token, or
        byte run, and that nothing was raised where Python could not
        propagate it. A producer worker may still be dropping a finished
        async call, so the counters get up to two seconds to settle."""
        kinds = ["objects", "callbacks", "iterators", "cancel tokens", "byte runs"]
        deadline = time.monotonic() + 2.0
        while True:
            gc.collect()
            live = [self.impl._debug_live(kind) for kind in range(len(kinds))]
            if not any(live) or time.monotonic() > deadline:
                break
            time.sleep(0.001)
        for label, count in zip(kinds, live):
            self.check(count == 0, f"{count} live {label} at exit")
        self.check(not _unraisable, f"unraisable exceptions: {_unraisable}")
        print(f"python/{self.name}: OK")
