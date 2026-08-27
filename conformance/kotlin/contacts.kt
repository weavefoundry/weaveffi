// Conformance consumer: contacts sample, Android/Kotlin (JNI) target.
//
// Exercises the original struct/enum/optional surface: `ContactBook` is a
// generated Closeable class (companion `invoke` for the canonical `new`
// constructor, instance methods, destroy through `close()`), `Contact` is a
// plain data class decoded from a value buffer (the nullable email rides
// inside the same buffer), enums cross as `ContactType` values, and
// `ContactsError` is a typed exception domain (`ContactsException` sealed
// subclasses extending the generic `WeaveFFIException`, raised through the
// payload-aware `fromCode` factory). Asserts the typed-error paths
// (InvalidName from an empty first name, NotFound from a missing id), record
// materialization with data-class value equality, the buffered nullable email
// parameter and field, list-of-record returns, boolean returns, and
// per-object state. Compiled in-module with the generated `WeaveFFI.kt`, so
// the `internal` constructor surface is reachable.
@file:JvmName("Main")

import com.weaveffi.Contact
import com.weaveffi.ContactBook
import com.weaveffi.ContactType
import com.weaveffi.ContactsException
import com.weaveffi.WeaveFFIException
import kotlin.system.exitProcess

fun expect(cond: Boolean, msg: String) {
    if (!cond) {
        System.err.println("assertion failed: $msg")
        exitProcess(1)
    }
}

/** Run `block` and return the exception it threw, or null if it completed. */
inline fun thrownBy(block: () -> Unit): Throwable? =
    try {
        block()
        null
    } catch (e: Throwable) {
        e
    }

fun main() {
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

        // Typed error from a method: a missing id reports NotFound (2), which
        // is both the sealed domain type and the generic brand exception.
        val missingErr = thrownBy { book.get(9999L) }
        expect(
            missingErr is ContactsException.NotFound,
            "get(9999) throws ContactsException.NotFound (got $missingErr)"
        )
        expect(missingErr is ContactsException, "NotFound is a ContactsException")
        expect(missingErr is WeaveFFIException, "NotFound is a WeaveFFIException")
        val missingCode = (missingErr as? WeaveFFIException)?.code
        expect(missingCode == 2, "NotFound code 2 (got $missingCode)")

        // Typed error: an empty first name is rejected with InvalidName (1)
        // and leaves the book unchanged.
        val invalidErr = thrownBy { book.add("", "Nameless", null, ContactType.Other) }
        expect(
            invalidErr is ContactsException.InvalidName,
            "add(\"\") throws ContactsException.InvalidName (got $invalidErr)"
        )
        val invalidCode = (invalidErr as? WeaveFFIException)?.code
        expect(invalidCode == 1, "InvalidName code 1 (got $invalidCode)")
        expect(book.count() == 1, "failed add leaves count == 1")

        // Each ContactBook object owns its own state.
        ContactBook().use { other ->
            expect(other.count() == 0, "fresh second book empty")
            expect(book.count() == 1, "first book unaffected")
        }
    }

    println("kotlin/contacts: OK")
}
