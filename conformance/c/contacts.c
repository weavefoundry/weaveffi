// Conformance consumer: contacts sample, C target.
//
// Includes the *generated* C header and links the contacts cdylib, exercising
// the ContactBook interface (constructor, methods, destroy), the Contact
// record (opaque handle + getters), enums, optional strings, lists, and the
// typed error-domain codes surfaced through the error-out convention. Exits 0
// on success; aborts (non-zero) on any failed assertion.

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"

int main(void) {
    weaveffi_error err = {0, NULL};

    // Interface constructor.
    weaveffi_contacts_ContactBook* book = weaveffi_contacts_ContactBook_new(&err);
    assert(err.code == 0);
    assert(book != NULL);

    // add -> owned Contact; methods take the receiver as the leading arg.
    weaveffi_contacts_Contact* alice = weaveffi_contacts_ContactBook_add(
        book, "Alice", "Smith", "alice@example.com",
        weaveffi_contacts_ContactType_Work, &err);
    assert(err.code == 0);
    assert(alice != NULL);
    int64_t alice_id = weaveffi_contacts_Contact_get_id(alice);
    assert(alice_id > 0);
    assert(weaveffi_contacts_Contact_get_contact_type(alice) ==
           weaveffi_contacts_ContactType_Work);
    weaveffi_contacts_Contact_destroy(alice);

    // get -> fresh Contact snapshot + string getters.
    weaveffi_contacts_Contact* c =
        weaveffi_contacts_ContactBook_get(book, alice_id, &err);
    assert(err.code == 0);
    assert(c != NULL);

    const char* first = weaveffi_contacts_Contact_get_first_name(c);
    assert(strcmp(first, "Alice") == 0);
    weaveffi_free_string(first);

    const char* email = weaveffi_contacts_Contact_get_email(c);
    assert(email != NULL && strcmp(email, "alice@example.com") == 0);
    weaveffi_free_string(email);

    weaveffi_contacts_Contact_destroy(c);

    // Optional string: null email round-trips as NULL.
    weaveffi_contacts_Contact* bob = weaveffi_contacts_ContactBook_add(
        book, "Bob", "Jones", NULL, weaveffi_contacts_ContactType_Personal, &err);
    assert(err.code == 0 && bob != NULL);
    int64_t bob_id = weaveffi_contacts_Contact_get_id(bob);
    assert(weaveffi_contacts_Contact_get_email(bob) == NULL);
    weaveffi_contacts_Contact_destroy(bob);

    // count + list.
    assert(weaveffi_contacts_ContactBook_count(book, &err) == 2);
    size_t len = 0;
    weaveffi_contacts_Contact** all =
        weaveffi_contacts_ContactBook_list(book, &len, &err);
    assert(err.code == 0);
    assert(len == 2);
    assert(all != NULL);
    for (size_t i = 0; i < len; i++) {
        weaveffi_contacts_Contact_destroy(all[i]);
    }

    // remove + typed error path: the domain code for a missing id is
    // ContactsError.NotFound (2), surfaced through the error-out slot.
    assert(weaveffi_contacts_ContactBook_remove(book, alice_id, &err));
    assert(weaveffi_contacts_ContactBook_count(book, &err) == 1);

    weaveffi_contacts_Contact* missing =
        weaveffi_contacts_ContactBook_get(book, 9999, &err);
    assert(missing == NULL);
    assert(err.code == weaveffi_contacts_ContactsError_NotFound);
    weaveffi_error_clear(&err);

    // Typed error path: empty first name reports ContactsError.InvalidName (1).
    weaveffi_contacts_Contact* bad = weaveffi_contacts_ContactBook_add(
        book, "", "Nameless", NULL, weaveffi_contacts_ContactType_Other, &err);
    assert(bad == NULL);
    assert(err.code == weaveffi_contacts_ContactsError_InvalidName);
    weaveffi_error_clear(&err);

    assert(weaveffi_contacts_ContactBook_remove(book, bob_id, &err));
    weaveffi_contacts_ContactBook_destroy(book);

    printf("c/contacts: OK\n");
    return 0;
}
