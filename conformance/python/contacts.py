"""Conformance consumer: contacts sample, Python target.

Exercises the generated ctypes wrapper end to end: the `ContactBook`
interface (its `__init__` calls the C constructor and `__del__` the destroy
symbol), enum marshalling, opaque struct handles with property getters,
optional strings, list-of-struct returns, boolean returns, and the typed
`ContactsError` subclasses (`InvalidName`, `NotFound`) raised by throwing
methods. The generated module is placed on sys.path via WV_PY; the cdylib is
selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import contacts as wv  # noqa: E402


def main() -> None:
    book = wv.ContactBook()
    assert book.count() == 0

    alice = book.add("Alice", "Smith", "alice@example.com", wv.ContactType.Work)
    assert alice.id > 0
    assert alice.first_name == "Alice"
    assert alice.last_name == "Smith"
    assert alice.email == "alice@example.com"
    assert alice.contact_type == wv.ContactType.Work

    # Optional string: a missing email round-trips as None.
    bob = book.add("Bob", "Jones", None, wv.ContactType.Personal)
    fetched = book.get(bob.id)
    assert fetched.email is None
    assert fetched.contact_type == wv.ContactType.Personal

    assert book.count() == 2
    everyone = book.list()
    assert len(everyone) == 2
    assert {p.first_name for p in everyone} == {"Alice", "Bob"}

    # Typed error: an empty name is rejected with the InvalidName class of
    # the ContactsError domain.
    try:
        book.add("", "Smith", None, wv.ContactType.Personal)
        raise AssertionError("expected InvalidName for empty first name")
    except wv.ContactsError.InvalidName as exc:
        assert exc.code == 1, exc.code
        assert isinstance(exc, wv.ContactsError)
        assert isinstance(exc, wv.WeaveFFIError)
    assert book.count() == 2

    assert book.remove(alice.id) is True
    assert book.remove(alice.id) is False
    assert book.count() == 1

    # Typed error: a missing id raises NotFound; the bare name is the same
    # class as the scoped alias.
    try:
        book.get(9999)
        raise AssertionError("expected NotFound for missing contact")
    except wv.NotFound as exc:
        assert exc.code == 2, exc.code
        assert isinstance(exc, wv.ContactsError)
    assert wv.NotFound is wv.ContactsError.NotFound

    # A second book is independent state: contacts don't leak across objects.
    other = wv.ContactBook()
    assert other.count() == 0

    print("python/contacts: OK")


main()
