"""Conformance consumer: events sample, Python target.

Exercises the full events surface: the ctypes CFUNCTYPE listener trampoline
(register -> fire synchronously on send -> unregister stops delivery) and the
opaque-iterator ABI (`next(iter, &out_item, &err) -> int32`) behind
`get_messages()`.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import events as wv  # noqa: E402


def main() -> None:
    received: list[str] = []
    sub = wv.register_message_listener(received.append)
    assert sub > 0, sub

    wv.send_message("alpha")
    wv.send_message("beta")
    assert received == ["alpha", "beta"], received

    wv.send_message("gamma")
    assert received == ["alpha", "beta", "gamma"], received

    msgs = list(wv.get_messages())
    assert msgs == ["alpha", "beta", "gamma"], msgs

    # Unregister stops delivery; messages still accumulate producer-side.
    wv.unregister_message_listener(sub)
    wv.send_message("delta")
    assert received == ["alpha", "beta", "gamma"], received
    assert list(wv.get_messages()) == ["alpha", "beta", "gamma", "delta"]

    print("python/events: OK")


main()
