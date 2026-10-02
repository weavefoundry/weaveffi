// Conformance consumer: contacts sample, Swift target.
//
// Binds through the generated `Contacts` package and asserts the interface
// surface: `ContactBook` as a final class whose `new` constructor is a plain
// `init()`, throwing methods (`add`, `get`) that raise the typed
// `ContactsError` domain enum, non-throwing methods (`list`, `remove`,
// `count`) called without `try`, enum marshalling, and `Contact` as a
// `Hashable` struct decoded from a value buffer. The nested `Contacts.Groups`
// namespace takes and returns the parent module's `ContactType` enum and
// `ContactBook` interface and throws the parent's domain; the sibling
// `Directory` root shares the `Contact` record and has its own domain. At
// exit, every native object and allocation has been released.

import CContacts
import Contacts
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

/// Every live-object counter the producer keeps must settle at zero.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<200 {
        live = (0..<5).map { contacts_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(10_000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

func run() throws {
    let book = ContactBook()
    expect(contacts_debug_live(0) == 1, "the leak counters see the live book")

    let alice = try book.add(
        firstName: "Alice", lastName: "Smith",
        email: "alice@example.com", contactType: .work)
    expect(alice.id > 0, "alice id positive")

    // The record is a value type: `get` decodes a fresh Contact struct equal
    // to the one `add` returned.
    let c = try book.get(id: alice.id)
    expect(c == alice, "get returns the added contact")
    expect(c.firstName == "Alice" && c.lastName == "Smith", "names")
    expect(c.email == "alice@example.com", "email")
    expect(c.contactType == .work, "contactType")

    // Optional string: a missing email round-trips as nil.
    let bob = try book.add(firstName: "Bob", lastName: "Jones", email: nil, contactType: .personal)
    let cb = try book.get(id: bob.id)
    expect(cb.email == nil, "bob email nil")
    expect(Set([alice, bob, cb]).count == 2, "Contact is Hashable")

    // The memberwise init builds a local value without touching the ABI.
    let local = Contact(id: 42, firstName: "Carol", lastName: "Doe", email: nil, contactType: .other)
    expect(local.id == 42 && local.email == nil, "memberwise init")

    // Non-throwing methods need no `try`.
    expect(book.count() == 2, "count == 2")
    let everyone = book.list()
    expect(everyone.map { $0.firstName }.sorted() == ["Alice", "Bob"], "list names")

    // A missing id raises the typed domain error's notFound case (code 2).
    do {
        _ = try book.get(id: 999)
        fail("expected ContactsError.notFound for missing contact")
    } catch let e as ContactsError {
        guard case let .notFound(message) = e else { fail("expected .notFound, got \(e)") }
        expect(e.errorCode == 2, "notFound code == 2 (got \(e.errorCode))")
        expect(message == "contact not found", "notFound message (got \(message))")
    }

    // An empty name raises the invalidName case (code 1).
    do {
        _ = try book.add(firstName: "", lastName: "Smith", email: nil, contactType: .personal)
        fail("expected ContactsError.invalidName for empty first name")
    } catch let e as ContactsError {
        guard case .invalidName = e else { fail("expected .invalidName, got \(e)") }
        expect(e.errorCode == 1, "invalidName code == 1 (got \(e.errorCode))")
    }
    expect(book.count() == 2, "rejected add stores nothing")

    // --- Nested module: the parent's enum, interface, and error domain ------
    _ = try book.add(firstName: "Dana", lastName: "Wu", email: nil, contactType: .work)
    expect(Contacts.Groups.countOfType(book: book, contactType: .work) == 2, "two work contacts")
    expect(Contacts.Groups.countOfType(book: book, contactType: .other) == 0, "no other contacts")
    expect(Contacts.Groups.dominantType(book: book) == .work, "work dominates")
    let work = Contacts.Groups.splitByType(book: book, contactType: .work)
    expect(work.count() == 2, "split book holds the work contacts")
    expect(work.list().allSatisfy { $0.contactType == .work }, "split book is all work")
    expect(book.count() == 3, "splitting leaves the source book intact")
    let firstWork = try Contacts.Groups.firstOfType(book: book, contactType: .work)
    expect(firstWork.firstName == "Alice", "first work contact (got \(firstWork.firstName))")
    do {
        _ = try Contacts.Groups.firstOfType(book: work, contactType: .personal)
        fail("expected ContactsError.notFound from the nested module")
    } catch ContactsError.notFound {
    }

    // --- Sibling root: shares the Contact record, has its own domain --------
    let card = Directory.card(contact: alice)
    expect(card == Card(displayName: "Smith, Alice", initials: "AS", hasEmail: true),
           "card (got \(card))")
    expect(!Directory.card(contact: bob).hasEmail, "bob's card has no email")
    let sorted = try Directory.sorted(contacts: [alice, bob, local])
    expect(sorted.map { $0.lastName } == ["Doe", "Jones", "Smith"], "sorted by last name")
    do {
        _ = try Directory.sorted(contacts: [])
        fail("expected DirectoryError.empty")
    } catch let e as DirectoryError {
        guard case .empty = e else { fail("expected .empty, got \(e)") }
        expect(e.errorCode == 1, "empty code == 1")
    }

    expect(book.remove(id: alice.id) == true, "remove returns true")
    expect(book.count() == 2, "count == 2 after remove")
}

do {
    try run()
} catch {
    fail("unexpected error: \(error)")
}
assertNoLeaks()
print("swift/contacts: OK")
