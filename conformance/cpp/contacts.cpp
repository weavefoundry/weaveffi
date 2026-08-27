// Conformance consumer: contacts sample, C++ target.
//
// Drives the generated header-only wrappers over the original struct/enum/
// optional surface: `ContactBook` is a move-only RAII interface class whose
// canonical `new` constructor maps to the default constructor (destroy runs
// in the destructor), `Contact` is a plain value struct decoded from a value
// buffer (the optional email rides inside the same buffer), enums cross as
// `enum class` values, and throwing wrappers surface the `ContactsError`
// domain hierarchy (`InvalidNameError`, `NotFoundError`). Aborts (non-zero)
// on any failed assertion.

#include <cassert>
#include <cstdio>
#include <optional>
#include <string>
#include <vector>

#include "weaveffi.hpp"

using namespace weaveffi;

int main() {
    // Canonical `new` constructor maps to the default constructor.
    ContactBook book;
    assert(book.count() == 0);

    // add -> Contact value struct; the present email crosses as a buffered
    // `string?` built from std::optional.
    Contact alice = book.add("Alice", "Smith", std::string("alice@example.com"),
                             ContactType::Work);
    assert(alice.id > 0);
    assert(alice.first_name == "Alice");
    assert(alice.last_name == "Smith");
    assert(alice.email.has_value() && *alice.email == "alice@example.com");
    assert(alice.contact_type == ContactType::Work);

    // get -> a fresh Contact snapshot decoded from a new buffer.
    Contact snap = book.get(alice.id);
    assert(snap.first_name == "Alice");
    assert(snap.last_name == "Smith");
    assert(snap.email.has_value() && *snap.email == "alice@example.com");

    // Optional string: an absent email round-trips as a disengaged optional.
    Contact bob = book.add("Bob", "Jones", std::nullopt, ContactType::Personal);
    assert(!bob.email.has_value());
    assert(bob.contact_type == ContactType::Personal);

    // count + list: the list return is one buffer decoded into a vector.
    assert(book.count() == 2);
    std::vector<Contact> everyone = book.list();
    assert(everyone.size() == 2);
    bool saw_alice = false, saw_bob = false;
    for (const auto& c : everyone) {
        if (c.first_name == "Alice") saw_alice = true;
        if (c.first_name == "Bob") saw_bob = true;
    }
    assert(saw_alice && saw_bob);

    // remove returns whether the id existed; a second remove reports false.
    assert(book.remove(alice.id));
    assert(!book.remove(alice.id));
    assert(book.count() == 1);

    // A missing id throws the per-code subclass; catching the domain base
    // proves the hierarchy (NotFoundError -> ContactsError -> WeaveFFIError).
    bool caught_missing = false;
    try {
        book.get(9999);
    } catch (const ContactsError& e) {
        caught_missing = (e.code() == 2);
    }
    assert(caught_missing);

    // The most-derived type is the per-code class.
    bool caught_typed = false;
    try {
        book.get(9999);
    } catch (const NotFoundError&) {
        caught_typed = true;
    }
    assert(caught_typed);

    // Typed error path: an empty first name reports InvalidNameError (1) and
    // leaves the book unchanged.
    bool caught_invalid = false;
    try {
        book.add("", "Nameless", std::nullopt, ContactType::Other);
    } catch (const InvalidNameError& e) {
        caught_invalid = (e.code() == 1);
    }
    assert(caught_invalid);
    assert(book.count() == 1);

    // Each ContactBook owns its own state.
    ContactBook other;
    assert(other.count() == 0);
    assert(book.count() == 1);

    // RAII: the destructors call weaveffi_contacts_ContactBook_destroy when
    // `book` and `other` leave scope; no explicit close.
    std::printf("cpp/contacts: OK\n");
    return 0;
}
