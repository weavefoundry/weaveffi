// Conformance consumer: contacts sample, C++ target.
//
// Drives the generated header-only wrappers over the struct/enum/optional
// surface and the module graph:
//  - `ContactBook` is an RAII interface class whose canonical `new`
//    constructor maps to the default constructor; `Contact` is a value struct
//    decoded from a value buffer; enums cross as `enum class` values; and
//    throwing wrappers surface the `ContactsError` hierarchy.
//  - `contacts::groups` is nested under the `contacts` root: its functions
//    take and return the parent's `ContactBook` interface and `ContactType`
//    enum, and report the parent's `ContactsError` domain.
//  - `directory` is a sibling root sharing the `Contact` record and declaring
//    its own `DirectoryError` domain.
// Ends by asserting the producer's leak counters are all zero.

#include <cstdio>
#include <optional>
#include <string>
#include <vector>

#include "check.hpp"
#include "contacts.hpp"

using namespace contacts;
namespace grp = ::contacts::contacts::groups;
namespace dir = ::contacts::directory;

static void run() {
    // Canonical `new` constructor maps to the default constructor.
    ContactBook book;
    CHECK(book.count() == 0);

    Contact alice = book.add("Alice", "Smith", std::string("alice@example.com"), ContactType::Work);
    CHECK(alice.id > 0);
    CHECK(alice.first_name == "Alice");
    CHECK(alice.last_name == "Smith");
    CHECK(alice.email.has_value() && *alice.email == "alice@example.com");
    CHECK(alice.contact_type == ContactType::Work);

    Contact snap = book.get(alice.id);
    CHECK(snap.first_name == "Alice" && snap.last_name == "Smith");
    CHECK(snap.email.has_value() && *snap.email == "alice@example.com");

    Contact bob = book.add("Bob", "Jones", std::nullopt, ContactType::Personal);
    CHECK(!bob.email.has_value());
    CHECK(bob.contact_type == ContactType::Personal);

    Contact carol = book.add("Carol", "Adams", std::nullopt, ContactType::Work);
    CHECK(book.count() == 3);
    std::vector<Contact> everyone = book.list();
    CHECK(everyone.size() == 3);

    // Typed errors: the per-code subclass derives from the domain and Error.
    bool caught = false;
    try {
        book.get(9999);
    } catch (const NotFoundError& e) {
        caught = e.code() == 2 && std::string(e.what()) == "contact not found";
        CHECK(dynamic_cast<const ContactsError*>(&e) != nullptr);
        CHECK(dynamic_cast<const Error*>(&e) != nullptr);
    }
    CHECK(caught);
    caught = false;
    try {
        book.add("", "Nameless", std::nullopt, ContactType::Other);
    } catch (const InvalidNameError& e) {
        caught = e.code() == 1;
    }
    CHECK(caught);
    CHECK(book.count() == 3);

    // The nested module uses the parent's interface and enum.
    CHECK(grp::count_of_type(book, ContactType::Work) == 2);
    CHECK(grp::count_of_type(book, ContactType::Other) == 0);
    CHECK(grp::dominant_type(book) == ContactType::Work);
    ContactBook work = grp::split_by_type(book, ContactType::Work);
    CHECK(work.count() == 2);
    CHECK(book.count() == 3);
    Contact first_personal = grp::first_of_type(book, ContactType::Personal);
    CHECK(first_personal.first_name == "Bob");
    // ...and reports the parent's error domain.
    caught = false;
    try {
        grp::first_of_type(work, ContactType::Personal);
    } catch (const NotFoundError& e) {
        caught = e.code() == 2;
    }
    CHECK(caught);

    // The sibling root shares the Contact record.
    Card card = dir::card(alice);
    CHECK(card.display_name == "Smith, Alice");
    CHECK(card.initials == "AS");
    CHECK(card.has_email);
    CHECK(!dir::card(bob).has_email);
    std::vector<Contact> sorted = dir::sorted({alice, bob, carol});
    CHECK(sorted.size() == 3);
    CHECK(sorted[0].last_name == "Adams" && sorted[1].last_name == "Jones" &&
          sorted[2].last_name == "Smith");
    caught = false;
    try {
        dir::sorted({});
    } catch (const EmptyError& e) {
        caught = e.code() == 1;
        CHECK(dynamic_cast<const DirectoryError*>(&e) != nullptr);
    }
    CHECK(caught);

    CHECK(book.remove(alice.id));
    CHECK(!book.remove(alice.id));
    CHECK(book.count() == 2);

    // Each ContactBook owns its own state.
    ContactBook other;
    CHECK(other.count() == 0);
}

int main() {
    run();
    check_no_leaks(contacts_debug_live, "contacts");
    std::printf("cpp/contacts: OK\n");
    return 0;
}
