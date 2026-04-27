"""End-to-end consumer test for the Python binding consumer.

Loads the calculator and contacts cdylibs at runtime via ctypes
(no generated bindings required) and exercises a representative
slice of the C ABI: add, create_contact, list_contacts,
delete_contact. Prints "OK" and exits 0 on success; any assertion
failure prints a diagnostic and exits 1.
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


calc = must_load("WEAVEFFI_LIB")
contacts = must_load("CONTACTS_LIB")

calc.weaveffi_calculator_add.argtypes = [
    ctypes.c_int32,
    ctypes.c_int32,
    ctypes.POINTER(WeaveffiError),
]
calc.weaveffi_calculator_add.restype = ctypes.c_int32

contacts.weaveffi_contacts_create_contact.argtypes = [
    ctypes.c_char_p,
    ctypes.c_char_p,
    ctypes.c_char_p,
    ctypes.c_int32,
    ctypes.POINTER(WeaveffiError),
]
contacts.weaveffi_contacts_create_contact.restype = ctypes.c_uint64

contacts.weaveffi_contacts_list_contacts.argtypes = [
    ctypes.POINTER(ctypes.c_size_t),
    ctypes.POINTER(WeaveffiError),
]
contacts.weaveffi_contacts_list_contacts.restype = ctypes.POINTER(ctypes.c_void_p)

contacts.weaveffi_contacts_Contact_get_id.argtypes = [ctypes.c_void_p]
contacts.weaveffi_contacts_Contact_get_id.restype = ctypes.c_int64

contacts.weaveffi_contacts_Contact_list_free.argtypes = [
    ctypes.POINTER(ctypes.c_void_p),
    ctypes.c_size_t,
]
contacts.weaveffi_contacts_Contact_list_free.restype = None

contacts.weaveffi_contacts_delete_contact.argtypes = [
    ctypes.c_uint64,
    ctypes.POINTER(WeaveffiError),
]
contacts.weaveffi_contacts_delete_contact.restype = ctypes.c_int32

contacts.weaveffi_contacts_count_contacts.argtypes = [ctypes.POINTER(WeaveffiError)]
contacts.weaveffi_contacts_count_contacts.restype = ctypes.c_int32


err = WeaveffiError()
total = calc.weaveffi_calculator_add(2, 3, ctypes.byref(err))
check(err.code == 0, "calculator_add error")
check(total == 5, "calculator_add(2,3) != 5")

err = WeaveffiError()
h = contacts.weaveffi_contacts_create_contact(
    b"Alice", b"Smith", b"alice@example.com", 0, ctypes.byref(err)
)
check(err.code == 0, "create_contact error")
check(h != 0, "create_contact returned 0")

err = WeaveffiError()
length = ctypes.c_size_t(0)
items = contacts.weaveffi_contacts_list_contacts(ctypes.byref(length), ctypes.byref(err))
check(err.code == 0, "list_contacts error")
check(length.value == 1, f"list_contacts length != 1 (got {length.value})")
check(bool(items), "list_contacts null")
check(
    contacts.weaveffi_contacts_Contact_get_id(items[0]) == h,
    "id mismatch",
)
contacts.weaveffi_contacts_Contact_list_free(items, length.value)

err = WeaveffiError()
deleted = contacts.weaveffi_contacts_delete_contact(h, ctypes.byref(err))
check(err.code == 0, "delete_contact error")
check(deleted == 1, "delete_contact did not return 1")

err = WeaveffiError()
remaining = contacts.weaveffi_contacts_count_contacts(ctypes.byref(err))
check(remaining == 0, "store not empty after cleanup")

print("OK")
