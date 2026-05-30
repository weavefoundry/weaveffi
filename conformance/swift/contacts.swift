// Conformance consumer: contacts sample, Swift target.
//
// Binds through the generated `WeaveFFI` module and asserts the full contacts
// surface: enum marshalling, opaque-handle classes with property getters,
// NUL-terminated string params (`withCString`), optional strings
// (`withOptionalCString`, null email), list-of-struct returns (the `out_len` +
// `T**` lowering), boolean returns, and the thrown-error path. Exercises the
// exact marshalling the Swift backend was previously generating incorrectly.

import Foundation
import WeaveFFI

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

do {
    let alice = try Contacts.contacts_create_contact("Alice", "Smith", "alice@example.com", .work)
    expect(alice > 0, "alice handle positive")

    let c = try Contacts.contacts_get_contact(alice)
    expect(c.first_name == "Alice", "first_name")
    expect(c.last_name == "Smith", "last_name")
    expect(c.email == "alice@example.com", "email")
    expect(c.contact_type == .work, "contact_type")

    // Optional string: a missing email round-trips as nil.
    let bob = try Contacts.contacts_create_contact("Bob", "Jones", nil, .personal)
    let cb = try Contacts.contacts_get_contact(bob)
    expect(cb.email == nil, "bob email nil")
    expect(cb.contact_type == .personal, "bob contact_type")

    expect(try Contacts.contacts_count_contacts() == 2, "count == 2")
    let everyone = try Contacts.contacts_list_contacts()
    expect(everyone.count == 2, "list count == 2")
    let names = everyone.map { $0.first_name }.sorted()
    expect(names == ["Alice", "Bob"], "list names")

    expect(try Contacts.contacts_delete_contact(alice) == true, "delete returns true")
    expect(try Contacts.contacts_count_contacts() == 1, "count == 1 after delete")

    // Error path throws a typed error with a non-zero code.
    do {
        _ = try Contacts.contacts_get_contact(9999)
        fail("expected WeaveFFIError for missing contact")
    } catch let WeaveFFIError.error(code, _) {
        expect(code != 0, "error code non-zero")
    }

    print("swift/contacts: OK")
} catch {
    FileHandle.standardError.write(Data("unexpected error: \(error)\n".utf8))
    exit(1)
}
