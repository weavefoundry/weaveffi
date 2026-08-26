// Conformance consumer: contacts sample, C target.
//
// Includes the *generated* C header and links the contacts cdylib, exercising
// the ContactBook interface (constructor, methods, destroy), the Contact
// record crossing the ABI as a serialized value buffer, enums, buffered
// optional strings, buffered lists, and the typed error-domain codes surfaced
// through the error-out convention. Exits 0 on success; aborts (non-zero) on
// any failed assertion.

#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"
#include "wvbuf.h"

// A decoded Contact record: id, first_name, last_name, email?, contact_type.
typedef struct {
    int64_t id;
    char* first_name;
    char* last_name;
    char* email;  // NULL when absent
    int32_t contact_type;
} contact_t;

static void read_contact(wv_reader* r, contact_t* c) {
    c->id = wv_get_i64(r);
    c->first_name = wv_get_str(r);
    c->last_name = wv_get_str(r);
    c->email = wv_get_bool(r) ? wv_get_str(r) : NULL;
    c->contact_type = wv_get_i32(r);
}

static void contact_free(contact_t* c) {
    free(c->first_name);
    free(c->last_name);
    free(c->email);
}

// Decode one Contact from an owned return buffer, then release the buffer.
static void take_contact(const uint8_t* ptr, size_t len, contact_t* c) {
    assert(ptr != NULL);
    wv_reader r;
    wv_r_init(&r, ptr, len);
    read_contact(&r, c);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ptr, len);
}

int main(void) {
    weaveffi_error err = {0};

    // Optional-string parameter encodings: present and absent.
    wv_writer some_email;
    wv_w_init(&some_email);
    wv_put_bool(&some_email, 1);
    wv_put_str(&some_email, "alice@example.com");
    const uint8_t no_email[1] = {0};

    // Interface constructor.
    weaveffi_contacts_ContactBook* book = weaveffi_contacts_ContactBook_new(&err);
    assert(err.code == 0);
    assert(book != NULL);

    // add -> Contact value buffer; methods take the receiver as the leading arg.
    size_t out_len = 0;
    const uint8_t* buf = weaveffi_contacts_ContactBook_add(
        book, "Alice", "Smith", some_email.buf, some_email.len,
        weaveffi_contacts_ContactType_Work, &out_len, &err);
    assert(err.code == 0);
    contact_t alice;
    take_contact(buf, out_len, &alice);
    assert(alice.id > 0);
    assert(strcmp(alice.first_name, "Alice") == 0);
    assert(alice.email != NULL && strcmp(alice.email, "alice@example.com") == 0);
    assert(alice.contact_type == weaveffi_contacts_ContactType_Work);
    int64_t alice_id = alice.id;
    contact_free(&alice);

    // get -> fresh Contact snapshot decoded from a new buffer.
    buf = weaveffi_contacts_ContactBook_get(book, alice_id, &out_len, &err);
    assert(err.code == 0);
    contact_t snap;
    take_contact(buf, out_len, &snap);
    assert(strcmp(snap.first_name, "Alice") == 0);
    assert(strcmp(snap.last_name, "Smith") == 0);
    contact_free(&snap);

    // Optional string: an absent email round-trips as the 0 flag byte.
    buf = weaveffi_contacts_ContactBook_add(
        book, "Bob", "Jones", no_email, sizeof no_email,
        weaveffi_contacts_ContactType_Personal, &out_len, &err);
    assert(err.code == 0);
    contact_t bob;
    take_contact(buf, out_len, &bob);
    assert(bob.email == NULL);
    int64_t bob_id = bob.id;
    contact_free(&bob);

    // count + list: the list return is one buffer holding u32 count + records.
    assert(weaveffi_contacts_ContactBook_count(book, &err) == 2);
    buf = weaveffi_contacts_ContactBook_list(book, &out_len, &err);
    assert(err.code == 0 && buf != NULL);
    wv_reader lr;
    wv_r_init(&lr, buf, out_len);
    uint32_t n = wv_get_u32(&lr);
    assert(n == 2);
    int seen_alice = 0, seen_bob = 0;
    for (uint32_t i = 0; i < n; i++) {
        contact_t c;
        read_contact(&lr, &c);
        if (strcmp(c.first_name, "Alice") == 0) seen_alice = 1;
        if (strcmp(c.first_name, "Bob") == 0) seen_bob = 1;
        contact_free(&c);
    }
    wv_r_expect_end(&lr);
    weaveffi_free_bytes((uint8_t*)buf, out_len);
    assert(seen_alice && seen_bob);

    // remove + typed error path: the domain code for a missing id is
    // ContactsError.NotFound (2), surfaced through the error-out slot.
    assert(weaveffi_contacts_ContactBook_remove(book, alice_id, &err));
    assert(weaveffi_contacts_ContactBook_count(book, &err) == 1);

    const uint8_t* missing =
        weaveffi_contacts_ContactBook_get(book, 9999, &out_len, &err);
    assert(missing == NULL);
    assert(err.code == weaveffi_contacts_ContactsError_NotFound);
    weaveffi_error_clear(&err);

    // Typed error path: empty first name reports ContactsError.InvalidName (1).
    const uint8_t* bad = weaveffi_contacts_ContactBook_add(
        book, "", "Nameless", no_email, sizeof no_email,
        weaveffi_contacts_ContactType_Other, &out_len, &err);
    assert(bad == NULL);
    assert(err.code == weaveffi_contacts_ContactsError_InvalidName);
    weaveffi_error_clear(&err);

    assert(weaveffi_contacts_ContactBook_remove(book, bob_id, &err));
    weaveffi_contacts_ContactBook_destroy(book);
    wv_w_free(&some_email);

    printf("c/contacts: OK\n");
    return 0;
}
