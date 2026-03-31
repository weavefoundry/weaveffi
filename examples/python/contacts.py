"""Python consumer example for the contacts sample.

Uses the auto-generated weaveffi Python bindings (ctypes) to exercise
the contacts API: create, list, get, and delete contacts.
"""

import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "generated", "python"))

from weaveffi import (  # noqa: E402
    ContactType,
    create_contact,
    count_contacts,
    delete_contact,
    get_contact,
    list_contacts,
)

TYPE_LABELS = {ContactType.Personal: "Personal", ContactType.Work: "Work", ContactType.Other: "Other"}


def print_contact(c):
    email = f" <{c.email}>" if c.email else ""
    label = TYPE_LABELS.get(c.contact_type, "Unknown")
    print(f"  [{c.id}] {c.first_name} {c.last_name}{email} ({label})")


def main():
    # 1. Create two contacts
    h1 = create_contact("Alice", "Smith", "alice@example.com", ContactType.Personal)
    print(f"Created contact #{h1}")

    h2 = create_contact("Bob", "Jones", None, ContactType.Work)
    print(f"Created contact #{h2}")

    print(f"\nTotal: {count_contacts()} contacts\n")

    # 2. List all contacts
    print("All contacts:")
    for c in list_contacts():
        print_contact(c)

    # 3. Get a contact by ID
    print(f"\nGet contact #{h1}:")
    contact = get_contact(h1)
    print_contact(contact)

    # 4. Delete a contact
    deleted = delete_contact(h2)
    print(f"\nDeleted contact #{h2}: {deleted}")
    print(f"Total: {count_contacts()} contacts\n")

    # 5. List remaining contacts
    print("Remaining contacts:")
    for c in list_contacts():
        print_contact(c)


if __name__ == "__main__":
    main()
