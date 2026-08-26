// Conformance consumer: contacts sample, Swift target.
//
// Binds through the generated `Contacts` module and asserts the 0.6.0
// interface surface: `ContactBook` as a final class whose `new` constructor is
// a plain `init()`, throwing methods (`add`, `get`) that raise the typed
// `ContactsError` domain enum, non-throwing methods (`list`, `remove`,
// `count`) called without `try`, real argument labels, enum marshalling, and
// `Contact` as a plain struct decoded from a value buffer: stored properties
// (no getters), an optional string field (nil email), and a list-of-struct
// return decoded to `[Contact]`. The typed-error asserts pin both the case
// and the numeric code carried by `errorCode`.

import Foundation
import Contacts

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

do {
    let book = ContactBook()

    let alice = try book.add(
        firstName: "Alice", lastName: "Smith",
        email: "alice@example.com", contactType: .work)
    expect(alice.id > 0, "alice id positive")

    // The record is a value type: `get` decodes a fresh Contact struct whose
    // stored properties round-trip the producer's field values.
    let c = try book.get(id: alice.id)
    expect(c.firstName == "Alice", "firstName")
    expect(c.lastName == "Smith", "lastName")
    expect(c.email == "alice@example.com", "email")
    expect(c.contactType == .work, "contactType")

    // Optional string: a missing email round-trips as nil.
    let bob = try book.add(firstName: "Bob", lastName: "Jones", email: nil, contactType: .personal)
    let cb = try book.get(id: bob.id)
    expect(cb.email == nil, "bob email nil")
    expect(cb.contactType == .personal, "bob contactType")

    // The memberwise init builds a local value without touching the ABI.
    let local = Contact(id: 42, firstName: "Carol", lastName: "Doe", email: nil, contactType: .other)
    expect(local.id == 42 && local.email == nil, "memberwise init")

    // Non-throwing methods need no `try`.
    expect(book.count() == 2, "count == 2")
    let everyone = book.list()
    expect(everyone.count == 2, "list count == 2")
    let names = everyone.map { $0.firstName }.sorted()
    expect(names == ["Alice", "Bob"], "list names")

    expect(book.remove(id: alice.id) == true, "remove returns true")
    expect(book.count() == 1, "count == 1 after remove")

    // A missing id raises the typed domain error's notFound case (code 2).
    do {
        _ = try book.get(id: 999)
        fail("expected ContactsError.notFound for missing contact")
    } catch let e as ContactsError {
        guard case let .notFound(message) = e else { fail("expected .notFound, got \(e)") }
        expect(e.errorCode == 2, "notFound code == 2 (got \(e.errorCode))")
        expect(!message.isEmpty, "notFound message non-empty")
    }

    // An empty name raises the invalidName case (code 1).
    do {
        _ = try book.add(firstName: "", lastName: "Smith", email: nil, contactType: .personal)
        fail("expected ContactsError.invalidName for empty first name")
    } catch let e as ContactsError {
        guard case .invalidName = e else { fail("expected .invalidName, got \(e)") }
        expect(e.errorCode == 1, "invalidName code == 1 (got \(e.errorCode))")
    }
    expect(book.count() == 1, "rejected add stores nothing")

    print("swift/contacts: OK")
} catch {
    FileHandle.standardError.write(Data("unexpected error: \(error)\n".utf8))
    exit(1)
}
