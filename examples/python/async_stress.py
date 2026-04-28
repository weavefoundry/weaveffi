"""Async stress test for the Python async lifecycle.

Loads the ``async-demo`` cdylib (path in ``ASYNC_DEMO_LIB``) and spawns
1000 concurrent calls to ``weaveffi_tasks_run_n_tasks_async`` via the C
ABI directly (so the test doesn't depend on the generator output).

Verifies:
  * every spawned worker fires its callback exactly once
  * each callback returns the ``n`` value passed in
  * after awaiting all calls, ``weaveffi_tasks_active_callbacks`` returns 0
    (the in-flight worker counter exposed by ``samples/async-demo``).

A leak in the Python wrapper would normally show up as a missing callback
(GC reclaimed the trampoline before the C side fired it), which would
hang or crash this test.

Prints ``OK`` and exits 0 on success; any failure prints a diagnostic and
exits 1.
"""

import ctypes
import os
import sys
import threading


N_TASKS = 1000


class WeaveffiError(ctypes.Structure):
    _fields_ = [("code", ctypes.c_int32), ("message", ctypes.c_char_p)]


def must_load(env_var: str) -> ctypes.CDLL:
    path = os.environ.get(env_var)
    if not path:
        sys.stderr.write(f"{env_var} not set\n")
        sys.exit(1)
    return ctypes.CDLL(path)


def check(cond: bool, msg: str) -> None:
    if not cond:
        sys.stderr.write(f"assertion failed: {msg}\n")
        sys.exit(1)


lib = must_load("ASYNC_DEMO_LIB")

CB_TYPE = ctypes.CFUNCTYPE(
    None,
    ctypes.c_void_p,
    ctypes.POINTER(WeaveffiError),
    ctypes.c_int32,
)

lib.weaveffi_tasks_run_n_tasks_async.argtypes = [
    ctypes.c_int32,
    CB_TYPE,
    ctypes.c_void_p,
]
lib.weaveffi_tasks_run_n_tasks_async.restype = None

lib.weaveffi_tasks_active_callbacks.argtypes = [ctypes.POINTER(WeaveffiError)]
lib.weaveffi_tasks_active_callbacks.restype = ctypes.c_int64

results: list[int] = [-1] * N_TASKS
ready = threading.Event()
remaining = N_TASKS
remaining_lock = threading.Lock()
callbacks: list = []  # Pin trampolines for the duration of the test.


def make_callback(idx: int):
    def _cb(ctx, err, value):
        global remaining
        results[idx] = int(value)
        with remaining_lock:
            remaining -= 1
            if remaining == 0:
                ready.set()
    cb = CB_TYPE(_cb)
    callbacks.append(cb)
    return cb


for i in range(N_TASKS):
    cb = make_callback(i)
    lib.weaveffi_tasks_run_n_tasks_async(i, cb, None)

if not ready.wait(timeout=30.0):
    sys.stderr.write(f"timeout waiting for callbacks; {remaining} still in flight\n")
    sys.exit(1)

for i, v in enumerate(results):
    check(v == i, f"results[{i}] = {v}, expected {i}")

err = WeaveffiError()
active = lib.weaveffi_tasks_active_callbacks(ctypes.byref(err))
check(err.code == 0, "active_callbacks returned error")
check(active == 0, f"expected active_callbacks == 0, got {active}")

print("OK")
