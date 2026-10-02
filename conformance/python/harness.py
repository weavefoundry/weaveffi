"""Shared helpers for the Python conformance consumers.

Every consumer imports the generated package as-is (conformance/python/run.sh
puts it on PYTHONPATH and selects the producer library through the loader's
`{PREFIX}_LIBRARY` override). This module adds the assertion helper, a hook
that turns any exception Python could only report as "unraisable" (raised in
a ctypes callback or a finalizer) into a failure, and the end-of-run leak
check against the producer's live-allocation counters.
"""
import gc
import sys
import types
from typing import Any, List

_unraisable: List[str] = []


def _record_unraisable(info: Any) -> None:
    _unraisable.append(f"{info.exc_type.__name__}: {info.exc_value} ({info.object!r})")


sys.unraisablehook = _record_unraisable


class Consumer:
    """Assertions for one consumer, labelled `python/<name>`."""

    def __init__(self, name: str) -> None:
        self.name = name

    def check(self, cond: bool, what: str) -> None:
        if not cond:
            print(f"python/{self.name}: FAIL: {what}", file=sys.stderr)
            sys.exit(1)

    def finish(self, package: types.ModuleType) -> None:
        """Collect garbage (running every finalizer), then require that the
        producer holds no live object, callback, iterator, cancel token, or
        returned allocation, and that nothing was raised where Python could
        not propagate it."""
        for _ in range(3):
            gc.collect()
        impl = sys.modules[f"{package.__name__}.{package.__name__}"]
        kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
        for kind, label in enumerate(kinds):
            live = impl._debug_live(kind)
            self.check(live == 0, f"{live} live {label} at exit")
        self.check(not _unraisable, f"unraisable exceptions: {_unraisable}")
        print(f"python/{self.name}: OK")
