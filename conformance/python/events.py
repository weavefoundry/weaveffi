"""Conformance consumer: events sample, Python target.

Validates that `events_get_messages()` drives the opaque-iterator ABI
(`next(iter, &out_item, &err) -> int32`) correctly from ctypes and yields the
messages in order. (Module-level listener registration is not yet emitted by
the Python backend, so only the send + iterate path is exercised here.)
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import events as wv  # noqa: E402


def main() -> None:
    wv.events_send_message("alpha")
    wv.events_send_message("beta")
    wv.events_send_message("gamma")

    msgs = list(wv.events_get_messages())
    assert msgs == ["alpha", "beta", "gamma"], msgs

    print("python/events: OK")


main()
