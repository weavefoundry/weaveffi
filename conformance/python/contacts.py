"""Conformance consumer: contacts sample, Python target.

Exercises the generated ctypes wrapper end to end: enum marshalling, opaque
struct handles with property getters, optional strings, list-of-struct returns,
boolean returns, and the raised-exception error path. The generated module is
placed on sys.path via WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import contacts as wv  # noqa: E402


def main() -> None:
    alice = wv.contacts_create_contact(
        "Alice", "Smith", "alice@example.com", wv.ContactType.Work
    )
    assert alice > 0

    c = wv.contacts_get_contact(alice)
    assert c.first_name == "Alice"
    assert c.last_name == "Smith"
    assert c.email == "alice@example.com"
    assert c.contact_type == wv.ContactType.Work

    # Optional string: a missing email round-trips as None.
    bob = wv.contacts_create_contact("Bob", "Jones", None, wv.ContactType.Personal)
    cb = wv.contacts_get_contact(bob)
    assert cb.email is None
    assert cb.contact_type == wv.ContactType.Personal

    assert wv.contacts_count_contacts() == 2
    everyone = wv.contacts_list_contacts()
    assert len(everyone) == 2
    assert {p.first_name for p in everyone} == {"Alice", "Bob"}

    assert wv.contacts_delete_contact(alice) is True
    assert wv.contacts_count_contacts() == 1

    # Error path raises a typed exception with a non-zero code.
    try:
        wv.contacts_get_contact(9999)
        raise AssertionError("expected WeaveFFIError for missing contact")
    except wv.WeaveFFIError as exc:
        assert exc.code != 0

    print("python/contacts: OK")


main()
