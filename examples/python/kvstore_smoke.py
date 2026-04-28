"""Kvstore consumer smoke test (Python / ctypes).

Loads ``KVSTORE_LIB`` at runtime and exercises the minimum lifecycle
that every language binding must support: open store, put a value,
get it back, delete it, close the store. Prints "OK" and exits 0
on success; any assertion failure prints a diagnostic and exits 1.
"""

import ctypes
import os
import sys


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


lib = must_load("KVSTORE_LIB")

lib.weaveffi_kv_open_store.argtypes = [ctypes.c_char_p, ctypes.POINTER(WeaveffiError)]
lib.weaveffi_kv_open_store.restype = ctypes.c_void_p

lib.weaveffi_kv_close_store.argtypes = [ctypes.c_void_p, ctypes.POINTER(WeaveffiError)]
lib.weaveffi_kv_close_store.restype = None

lib.weaveffi_kv_put.argtypes = [
    ctypes.c_void_p,
    ctypes.c_char_p,
    ctypes.POINTER(ctypes.c_uint8),
    ctypes.c_size_t,
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_int64),
    ctypes.POINTER(WeaveffiError),
]
lib.weaveffi_kv_put.restype = ctypes.c_bool

lib.weaveffi_kv_get.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.POINTER(WeaveffiError)]
lib.weaveffi_kv_get.restype = ctypes.c_void_p

lib.weaveffi_kv_Entry_get_key.argtypes = [ctypes.c_void_p]
lib.weaveffi_kv_Entry_get_key.restype = ctypes.c_char_p

lib.weaveffi_kv_Entry_get_value.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t)]
lib.weaveffi_kv_Entry_get_value.restype = ctypes.POINTER(ctypes.c_uint8)

lib.weaveffi_kv_Entry_destroy.argtypes = [ctypes.c_void_p]
lib.weaveffi_kv_Entry_destroy.restype = None

lib.weaveffi_kv_delete.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.POINTER(WeaveffiError)]
lib.weaveffi_kv_delete.restype = ctypes.c_bool

lib.weaveffi_free_string.argtypes = [ctypes.c_char_p]
lib.weaveffi_free_string.restype = None

lib.weaveffi_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
lib.weaveffi_free_bytes.restype = None


err = WeaveffiError()
store = lib.weaveffi_kv_open_store(b"/tmp/kvstore-smoke", ctypes.byref(err))
check(err.code == 0, "open_store error")
check(store is not None, "open_store returned null")

err = WeaveffiError()
payload = (ctypes.c_uint8 * 5)(*b"hello")
ok = lib.weaveffi_kv_put(
    store, b"greeting", payload, 5, 1, None, ctypes.byref(err)
)
check(err.code == 0, "put error")
check(bool(ok), "put returned false")

err = WeaveffiError()
entry = lib.weaveffi_kv_get(store, b"greeting", ctypes.byref(err))
check(err.code == 0, "get error")
check(entry is not None, "get returned null")

length = ctypes.c_size_t(0)
value_ptr = lib.weaveffi_kv_Entry_get_value(entry, ctypes.byref(length))
check(length.value == 5, f"value length mismatch (got {length.value})")
got = bytes(value_ptr[: length.value])
check(got == b"hello", f"value mismatch (got {got!r})")
lib.weaveffi_free_bytes(value_ptr, length.value)
lib.weaveffi_kv_Entry_destroy(entry)

err = WeaveffiError()
deleted = lib.weaveffi_kv_delete(store, b"greeting", ctypes.byref(err))
check(err.code == 0, "delete error")
check(bool(deleted), "delete did not return true")

err = WeaveffiError()
lib.weaveffi_kv_close_store(store, ctypes.byref(err))
check(err.code == 0, "close_store error")

print("OK")
