"""Conformance consumer: contacts sample, Python target.

Exercises the generated ctypes wrapper end to end: the reference-counted
`ContactBook` interface (its `__init__` calls the C constructor; `close()`,
the `with` statement, or `__del__` release the reference), enum marshalling,
the `Contact` record decoded from a value buffer into a plain dataclass with
value equality, optional strings (a buffered `string?` parameter and field),
list-of-record returns, boolean returns, the typed `ContactsError`
subclasses raised by throwing methods, and cross-module types: the nested
`contacts.groups` module taking and returning the parent's `ContactBook`,
`ContactType`, and `Contact`, and the sibling `directory` module taking a
`Contact` record and raising its own `DirectoryError` domain. Ends with the
leak check (see harness.py).
"""
import contacts as wv
from harness import Consumer

consumer = Consumer("contacts")
check = consumer.check


def book_basics() -> None:
    book = wv.ContactBook()
    check(book.count() == 0, "fresh book is empty")

    alice = book.add("Alice", "Smith", "alice@example.com", wv.ContactType.Work)
    check(alice.id > 0 and alice.first_name == "Alice" and alice.last_name == "Smith",
          f"alice {alice}")
    check(alice.email == "alice@example.com" and alice.contact_type == wv.ContactType.Work,
          f"alice email and type {alice}")

    # Optional string: a missing email round-trips as None. Records are
    # plain dataclass values, so a fetched contact compares equal to a
    # locally constructed one field by field.
    bob = book.add("Bob", "Jones", None, wv.ContactType.Personal)
    fetched = book.get(bob.id)
    check(fetched == wv.Contact(id=bob.id, first_name="Bob", last_name="Jones", email=None,
                                contact_type=wv.ContactType.Personal), f"fetched {fetched}")
    # Strings cross as pointer + length, so an interior NUL survives.
    nul = book.add("Nul\0Name", "Tail\0", "a\0b", wv.ContactType.Other)
    check(book.get(nul.id).first_name == "Nul\0Name" and nul.email == "a\0b",
          f"interior NUL round trip {nul}")
    check(book.remove(nul.id) is True, "remove nul")

    check(book.count() == 2, "two contacts")
    everyone = book.list()
    check({p.first_name for p in everyone} == {"Alice", "Bob"}, f"list {everyone}")

    # Typed errors carry stable codes and the class hierarchy.
    try:
        book.add("", "Smith", None, wv.ContactType.Personal)
        check(False, "expected InvalidName for an empty first name")
    except wv.ContactsError.InvalidName as exc:
        check(exc.code == 1 and isinstance(exc, wv.ContactsError) and isinstance(exc, wv.Error),
              f"InvalidName {exc!r}")
        check(exc.message == "name must not be empty", f"InvalidName message {exc.message!r}")
    check(book.count() == 2, "failed add left the book unchanged")

    check(book.remove(alice.id) is True, "remove alice")
    check(book.remove(alice.id) is False, "remove alice twice")
    check(book.count() == 1, "one contact left")

    try:
        book.get(9999)
        check(False, "expected NotFound for a missing contact")
    except wv.NotFound as exc:
        check(exc.code == 2 and isinstance(exc, wv.ContactsError), f"NotFound {exc!r}")
    check(wv.NotFound is wv.ContactsError.NotFound, "bare NotFound is the scoped alias")

    # Objects are context managers with an idempotent close(); a closed
    # wrapper rejects further use.
    with wv.ContactBook() as other:
        check(other.count() == 0, "second book is independent")
        other.close()
    other.close()
    try:
        other.count()
        check(False, "expected a use-after-close error")
    except wv.Error as exc:
        check("after close" in exc.message, f"use-after-close message {exc.message!r}")
    book.close()


def nested_and_sibling_modules() -> None:
    book = wv.ContactBook()
    work = book.add("Wanda", "Work", None, wv.ContactType.Work)
    book.add("Walt", "Work", "w@example.com", wv.ContactType.Work)
    book.add("Pat", "Home", None, wv.ContactType.Personal)

    # contacts.groups takes the parent module's interface and enum.
    check(wv.count_of_type(book, wv.ContactType.Work) == 2, "count_of_type Work")
    check(wv.count_of_type(book, wv.ContactType.Other) == 0, "count_of_type Other")
    dominant = wv.dominant_type(book)
    check(dominant is wv.ContactType.Work, f"dominant_type {dominant!r}")

    # ... and returns the parent's interface and record.
    split = wv.split_by_type(book, wv.ContactType.Work)
    check(isinstance(split, wv.ContactBook) and split.count() == 2, "split_by_type")
    check(book.count() == 3, "split_by_type copies instead of moving")
    first = wv.first_of_type(book, wv.ContactType.Work)
    check(first == work, f"first_of_type {first}")
    try:
        wv.first_of_type(split, wv.ContactType.Personal)
        check(False, "expected NotFound from the nested module")
    except wv.ContactsError.NotFound as exc:
        check(exc.code == 2, f"nested NotFound {exc!r}")
    split.close()

    # The sibling directory module takes a contacts record by value.
    card = wv.card(first)
    check(card == wv.Card(display_name="Work, Wanda", initials="WW", has_email=False),
          f"card {card}")
    ordered = wv.sorted(book.list())
    check([c.first_name for c in ordered] == ["Pat", "Walt", "Wanda"],
          f"sorted {[c.first_name for c in ordered]}")
    try:
        wv.sorted([])
        check(False, "expected DirectoryError.Empty")
    except wv.DirectoryError.Empty as exc:
        check(exc.code == 1 and not isinstance(exc, wv.ContactsError),
              f"Empty is its own domain {exc!r}")
        check(exc.message == "no contacts to list", f"Empty message {exc.message!r}")
    book.close()


def main() -> None:
    book_basics()
    nested_and_sibling_modules()
    consumer.finish(wv)


main()
