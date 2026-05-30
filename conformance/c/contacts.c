// Conformance consumer: contacts sample, C target.
//
// Includes the *generated* C header and links the contacts cdylib, exercising
// structs (opaque handle + getters), enums, optional strings, lists, handles,
// and the error-out convention. Exits 0 on success; aborts (non-zero) on any
// failed assertion.

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"

int main(void) {
    weaveffi_error err = {0, NULL};

    // create_contact -> handle
    weaveffi_handle_t alice = weaveffi_contacts_create_contact(
        "Alice", "Smith", "alice@example.com",
        weaveffi_contacts_ContactType_Work, &err);
    assert(err.code == 0);
    assert(alice > 0);

    // get_contact -> opaque struct + getters
    weaveffi_contacts_Contact* c = weaveffi_contacts_get_contact(alice, &err);
    assert(err.code == 0);
    assert(c != NULL);

    assert(weaveffi_contacts_Contact_get_id(c) == (int64_t)alice);
    assert(weaveffi_contacts_Contact_get_contact_type(c) ==
           weaveffi_contacts_ContactType_Work);

    const char* first = weaveffi_contacts_Contact_get_first_name(c);
    assert(strcmp(first, "Alice") == 0);
    weaveffi_free_string(first);

    const char* email = weaveffi_contacts_Contact_get_email(c);
    assert(email != NULL && strcmp(email, "alice@example.com") == 0);
    weaveffi_free_string(email);

    weaveffi_contacts_Contact_destroy(c);

    // optional string: null email round-trips as NULL
    weaveffi_handle_t bob = weaveffi_contacts_create_contact(
        "Bob", "Jones", NULL, weaveffi_contacts_ContactType_Personal, &err);
    assert(err.code == 0);
    weaveffi_contacts_Contact* cb = weaveffi_contacts_get_contact(bob, &err);
    assert(cb != NULL);
    assert(weaveffi_contacts_Contact_get_email(cb) == NULL);
    weaveffi_contacts_Contact_destroy(cb);

    // count + list
    assert(weaveffi_contacts_count_contacts(&err) == 2);
    size_t len = 0;
    weaveffi_contacts_Contact** all = weaveffi_contacts_list_contacts(&len, &err);
    assert(err.code == 0);
    assert(len == 2);
    assert(all != NULL);
    for (size_t i = 0; i < len; i++) {
        weaveffi_contacts_Contact_destroy(all[i]);
    }

    // delete + error path
    assert(weaveffi_contacts_delete_contact(alice, &err));
    assert(weaveffi_contacts_count_contacts(&err) == 1);

    weaveffi_contacts_Contact* missing = weaveffi_contacts_get_contact(9999, &err);
    assert(missing == NULL);
    assert(err.code != 0);
    weaveffi_error_clear(&err);

    printf("c/contacts: OK\n");
    return 0;
}
