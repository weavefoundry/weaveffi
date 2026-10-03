// Conformance consumer: contacts sample, Kotlin (JVM via JNI) target.
//
// Exercises the struct/enum/optional surface across three modules:
// `ContactBook` is a generated `AutoCloseable` class (companion `invoke` for
// the `new` constructor, instance methods, release through `close()`),
// `Contact` is a data class decoded from a value buffer (the nullable email
// rides inside it), enums cross as `ContactType` values, and `ContactsError`
// is a typed exception domain (`ContactsException` sealed subclasses
// extending `FfiException`). The nested `contacts.groups` module
// (`Contacts.Groups`) takes the parent's interface and C-style enum, returns
// a new parent-module object and record, and reports the parent's domain; the
// sibling top-level `directory` module (`Directory`) takes the `contacts`
// record by value, alone and in a list, and reports its own domain. At exit
// it checks that every native resource was released.
@file:JvmName("Main")

import contacts.Card
import contacts.Contact
import contacts.ContactBook
import contacts.ContactType
import contacts.Contacts
import contacts.ContactsException
import contacts.Directory
import contacts.DirectoryException
import contacts.FfiException
import contacts.JniBridge

fun checkBook() {
    ContactBook().use { book ->
        expect(book.count() == 0, "fresh book empty")

        // add() returns the stored record with its assigned id.
        val alice = book.add("Alice", "Smith", "alice@example.com", ContactType.Work)
        expect(alice.id > 0L, "alice id positive")
        expect(alice.first_name == "Alice", "alice first_name (got ${alice.first_name})")
        expect(alice.last_name == "Smith", "alice last_name")
        expect(alice.email == "alice@example.com", "alice email")
        expect(alice.contact_type == ContactType.Work, "alice contact_type")

        // Nullable email: a missing value round-trips as null. Records are
        // plain data classes, so a fetched contact compares equal to a
        // locally constructed one field by field.
        val bob = book.add("Bob", "Jones", null, ContactType.Personal)
        val fetched = book.get(bob.id)
        expect(fetched.email == null, "bob email null (got ${fetched.email})")
        expect(fetched.contact_type == ContactType.Personal, "bob contact_type")
        expect(
            fetched == Contact(bob.id, "Bob", "Jones", null, ContactType.Personal),
            "fetched bob equals constructed Contact (got $fetched)"
        )

        expect(book.count() == 2, "count == 2")
        val everyone = book.list()
        expect(everyone.size == 2, "list length == 2 (got ${everyone.size})")
        expect(
            everyone.map { it.first_name }.toSet() == setOf("Alice", "Bob"),
            "list names (got ${everyone.map { it.first_name }})"
        )

        // remove returns whether the id existed; a second remove reports false.
        expect(book.remove(alice.id), "remove returns true")
        expect(!book.remove(alice.id), "second remove returns false")
        expect(book.count() == 1, "count == 1 after remove")

        // Typed error from a method: a missing id reports NotFound (2).
        val missingErr = thrownBy { book.get(9999L) }
        expect(
            missingErr is ContactsException.NotFound,
            "get(9999) throws ContactsException.NotFound (got $missingErr)"
        )
        expect(missingErr is FfiException, "NotFound is an FfiException")
        val missingCode = (missingErr as? FfiException)?.code
        expect(missingCode == 2, "NotFound code 2 (got $missingCode)")

        // Typed error: an empty first name is rejected with InvalidName (1)
        // and leaves the book unchanged.
        val invalidErr = thrownBy { book.add("", "Nameless", null, ContactType.Other) }
        expect(
            invalidErr is ContactsException.InvalidName,
            "add(\"\") throws ContactsException.InvalidName (got $invalidErr)"
        )
        val invalidCode = (invalidErr as? FfiException)?.code
        expect(invalidCode == 1, "InvalidName code 1 (got $invalidCode)")
        expect(book.count() == 1, "failed add leaves count == 1")

        // Each ContactBook object owns its own state.
        ContactBook().use { other ->
            expect(other.count() == 0, "fresh second book empty")
            expect(book.count() == 1, "first book unaffected")
        }
    }
}

fun checkGroups() {
    ContactBook().use { book ->
        book.add("Ann", "Lee", null, ContactType.Work)
        book.add("Ben", "Ito", "ben@example.com", ContactType.Work)
        book.add("Cy", "Ng", null, ContactType.Personal)

        // The nested module takes the parent's interface and enum.
        expect(Contacts.Groups.countOfType(book, ContactType.Work) == 2, "two work contacts")
        expect(Contacts.Groups.countOfType(book, ContactType.Other) == 0, "no other contacts")
        expect(Contacts.Groups.dominantType(book) == ContactType.Work, "work dominates")

        // It returns a new object of the parent module's interface type.
        Contacts.Groups.splitByType(book, ContactType.Work).use { work ->
            expect(work.count() == 2, "split book holds the work contacts (got ${work.count()})")
            expect(work.list().all { it.contact_type == ContactType.Work }, "split book is all work")
            expect(book.count() == 3, "source book unchanged")
        }

        // And the parent's record and error domain.
        val first = Contacts.Groups.firstOfType(book, ContactType.Personal)
        expect(first.first_name == "Cy", "first personal contact (got $first)")
        val none = thrownBy { Contacts.Groups.firstOfType(book, ContactType.Other) }
        expect(none is ContactsException.NotFound, "firstOfType(Other) throws NotFound (got $none)")
    }
    ContactBook().use { empty ->
        expect(Contacts.Groups.dominantType(empty) == ContactType.Personal, "empty book defaults to Personal")
    }
}

fun checkDirectory() {
    // A sibling top-level module taking the `contacts` record by value.
    val ann = Contact(1L, "Ann", "Lee", "ann@example.com", ContactType.Work)
    val cy = Contact(2L, "Cy", "Ng", null, ContactType.Personal)
    expect(Directory.card(ann) == Card("Lee, Ann", "AL", true), "card(ann) (got ${Directory.card(ann)})")
    expect(Directory.card(cy) == Card("Ng, Cy", "CN", false), "card(cy)")
    val zed = Contact(3L, "Zed", "Abe", null, ContactType.Other)
    expect(Directory.sorted(listOf(cy, ann, zed)) == listOf(zed, ann, cy), "sorted by last name")
    val empty = thrownBy { Directory.sorted(listOf()) }
    expect(empty is DirectoryException.Empty, "sorted(empty) throws DirectoryException.Empty (got $empty)")
    expect((empty as FfiException).code == 1, "Empty code 1")
    expect(empty !is ContactsException, "the directory domain is its own")
}

fun main() {
    checkBook()
    checkGroups()
    checkDirectory()
    println("kotlin/contacts: OK")
    expectNoLeaks { JniBridge.debug_live(it) }
    println("kotlin/contacts: no leaks")
}
