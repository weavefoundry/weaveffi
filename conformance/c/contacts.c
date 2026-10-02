// Conformance consumer: contacts sample, C target (ABI revision 3).
//
// Includes the generated `contacts.h` and `contacts_buffer.h` and links the
// contacts cdylib. Covers the load-time contract check (ABI revision and
// both roots' checksums), the ContactBook interface (constructor, methods,
// destroy), (ptr, len) string parameters, the Contact record and its
// optional email crossing as value buffers, typed error codes and messages,
// the nested `contacts.groups` module (which uses the parent's C-style enum,
// interface, and error domain), and the sibling `directory` root sharing the
// Contact record. Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include "contacts_buffer.h"

static void check_contract(void) {
    assert(CONTACTS_ABI_VERSION == 3u);
    assert(contacts_abi_version() == CONTACTS_ABI_VERSION);
    assert(contacts_contacts_checksum() == CONTACTS_CONTACTS_CHECKSUM);
    assert(contacts_directory_checksum() == CONTACTS_DIRECTORY_CHECKSUM);
    assert(CONTACTS_CONTACTS_CHECKSUM != CONTACTS_DIRECTORY_CHECKSUM);
}

// Decode an owned Contact return, release the buffer, and return the value
// (which the caller frees with contacts_contacts_Contact_free).
static contacts_contacts_Contact take_contact(const uint8_t* ptr, size_t len) {
    assert(ptr != NULL);
    contacts_contacts_Contact c;
    assert(contacts_contacts_Contact_decode(ptr, len, &c));
    contacts_free_bytes((uint8_t*)ptr, len);
    return c;
}

// Add a contact (with an optional email), returning the stored record.
static contacts_contacts_Contact add(contacts_contacts_ContactBook* book, const char* first,
                                     const char* last, const char* email,
                                     contacts_contacts_ContactType type) {
    contacts_error err = {0};
    contacts_str email_view = contacts_str_of(email);
    contacts_writer w;
    memset(&w, 0, sizeof w);
    contacts_opt_string_write(&w, email ? &email_view : NULL);
    size_t len = 0;
    const uint8_t* buf = contacts_contacts_ContactBook_add(book, STR(first), STR(last), w.ptr,
                                                           w.len, type, &len, &err);
    assert(err.code == 0);
    contacts_writer_free(&w);
    return take_contact(buf, len);
}

static void book_surface(void) {
    contacts_error err = {0};
    contacts_contacts_ContactBook* book = contacts_contacts_ContactBook_new(&err);
    assert(err.code == 0 && book != NULL);

    contacts_contacts_Contact alice =
        add(book, "Alice", "Smith", "alice@example.com", contacts_contacts_ContactType_Work);
    assert(alice.id > 0);
    assert(strcmp(alice.first_name.ptr, "Alice") == 0);
    assert(strcmp(alice.last_name.ptr, "Smith") == 0);
    assert(alice.email != NULL && strcmp(alice.email->ptr, "alice@example.com") == 0);
    assert(alice.contact_type == contacts_contacts_ContactType_Work);

    // get -> a fresh snapshot of the same record.
    size_t len = 0;
    const uint8_t* buf = contacts_contacts_ContactBook_get(book, alice.id, &len, &err);
    assert(err.code == 0);
    contacts_contacts_Contact snap = take_contact(buf, len);
    assert(snap.id == alice.id && strcmp(snap.last_name.ptr, "Smith") == 0);
    contacts_contacts_Contact_free(&snap);

    // An absent email round-trips as an absent optional.
    contacts_contacts_Contact bob =
        add(book, "Bob", "Jones", NULL, contacts_contacts_ContactType_Personal);
    assert(bob.email == NULL);

    // A non-ASCII name crosses intact.
    contacts_contacts_Contact zoe =
        add(book, "Zo\xC3\xAB", "Quinn", NULL, contacts_contacts_ContactType_Other);
    assert(zoe.first_name.len == 4 && strcmp(zoe.first_name.ptr, "Zo\xC3\xAB") == 0);

    // count + list: the list return is one buffer holding every record.
    assert(contacts_contacts_ContactBook_count(book, &err) == 3);
    buf = contacts_contacts_ContactBook_list(book, &len, &err);
    assert(err.code == 0 && buf != NULL);
    contacts_list_contacts_Contact all;
    assert(contacts_list_contacts_Contact_decode(buf, len, &all));
    contacts_free_bytes((uint8_t*)buf, len);
    assert(all.len == 3);
    assert(strcmp(all.items[0].first_name.ptr, "Alice") == 0);
    assert(strcmp(all.items[1].first_name.ptr, "Bob") == 0);
    assert(all.items[2].id == zoe.id);
    contacts_list_contacts_Contact_free(&all);

    // remove, then the typed NotFound error for the missing id.
    assert(contacts_contacts_ContactBook_remove(book, alice.id, &err));
    assert(!contacts_contacts_ContactBook_remove(book, alice.id, &err));
    assert(contacts_contacts_ContactBook_count(book, &err) == 2);
    assert(contacts_contacts_ContactBook_get(book, alice.id, &len, &err) == NULL);
    assert(err.code == contacts_contacts_ContactsError_NotFound);
    assert(strcmp(err.message, "contact not found") == 0);
    contacts_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    // An empty first name reports InvalidName. A NULL pointer with length 0
    // is the empty string.
    const uint8_t absent[1] = {0};
    buf = contacts_contacts_ContactBook_add(book, NULL, 0, STR("Nameless"), BYTES(absent),
                                            contacts_contacts_ContactType_Other, &len, &err);
    assert(buf == NULL);
    assert(err.code == contacts_contacts_ContactsError_InvalidName);
    assert(strcmp(err.message, "name must not be empty") == 0);
    contacts_error_clear(&err);

    // A NULL pointer with a nonzero length is a marshalling failure (-3).
    buf = contacts_contacts_ContactBook_add(book, NULL, 3, STR("X"), BYTES(absent),
                                            contacts_contacts_ContactType_Other, &len, &err);
    assert(buf == NULL && err.code == -3);
    contacts_error_clear(&err);

    contacts_contacts_Contact_free(&alice);
    contacts_contacts_Contact_free(&bob);
    contacts_contacts_Contact_free(&zoe);
    contacts_contacts_ContactBook_destroy(book);
}

// contacts.groups: nested under the contacts root, so its functions take
// the parent's ContactBook and ContactType and report the parent's errors.
static void nested_module(void) {
    contacts_error err = {0};
    contacts_contacts_ContactBook* book = contacts_contacts_ContactBook_new(&err);
    contacts_contacts_Contact c[3] = {
        add(book, "Ann", "Lee", NULL, contacts_contacts_ContactType_Work),
        add(book, "Bo", "Kim", "bo@example.com", contacts_contacts_ContactType_Work),
        add(book, "Cy", "Ng", NULL, contacts_contacts_ContactType_Personal),
    };

    assert(contacts_contacts_groups_count_of_type(book, contacts_contacts_ContactType_Work,
                                                  &err) == 2);
    assert(contacts_contacts_groups_count_of_type(book, contacts_contacts_ContactType_Other,
                                                  &err) == 0);
    assert(err.code == 0);
    assert(contacts_contacts_groups_dominant_type(book, &err) ==
           contacts_contacts_ContactType_Work);

    contacts_contacts_ContactBook* work =
        contacts_contacts_groups_split_by_type(book, contacts_contacts_ContactType_Work, &err);
    assert(err.code == 0 && work != NULL && work != book);
    assert(contacts_contacts_ContactBook_count(work, &err) == 2);
    assert(contacts_contacts_groups_dominant_type(work, &err) ==
           contacts_contacts_ContactType_Work);
    contacts_contacts_ContactBook_destroy(work);

    size_t len = 0;
    const uint8_t* buf = contacts_contacts_groups_first_of_type(
        book, contacts_contacts_ContactType_Work, &len, &err);
    assert(err.code == 0);
    contacts_contacts_Contact first = take_contact(buf, len);
    assert(strcmp(first.first_name.ptr, "Ann") == 0);
    contacts_contacts_Contact_free(&first);

    // The parent domain's NotFound code, reported from the nested module.
    buf = contacts_contacts_groups_first_of_type(book, contacts_contacts_ContactType_Other,
                                                 &len, &err);
    assert(buf == NULL);
    assert(err.code == contacts_contacts_ContactsError_NotFound);
    contacts_error_clear(&err);

    // An out-of-range enum discriminant is a marshalling failure (-3).
    assert(contacts_contacts_groups_count_of_type(book, (contacts_contacts_ContactType)9,
                                                  &err) == 0);
    assert(err.code == -3);
    contacts_error_clear(&err);

    for (int i = 0; i < 3; i++) contacts_contacts_Contact_free(&c[i]);
    contacts_contacts_ContactBook_destroy(book);
}

// directory: a sibling root that shares only the Contact record.
static void sibling_root(void) {
    contacts_error err = {0};
    contacts_contacts_ContactBook* book = contacts_contacts_ContactBook_new(&err);
    contacts_contacts_Contact zoe =
        add(book, "Zoe", "Quinn", NULL, contacts_contacts_ContactType_Personal);
    contacts_contacts_Contact ada =
        add(book, "Ada", "Byron", "ada@example.com", contacts_contacts_ContactType_Work);
    contacts_contacts_ContactBook_destroy(book);

    // card(Contact) -> Card: a record built in one root, read by the other.
    contacts_writer w;
    memset(&w, 0, sizeof w);
    contacts_contacts_Contact_write(&w, &ada);
    size_t len = 0;
    const uint8_t* buf = contacts_directory_card(w.ptr, w.len, &len, &err);
    contacts_writer_free(&w);
    assert(err.code == 0 && buf != NULL);
    contacts_directory_Card card;
    assert(contacts_directory_Card_decode(buf, len, &card));
    contacts_free_bytes((uint8_t*)buf, len);
    assert(strcmp(card.display_name.ptr, "Byron, Ada") == 0);
    assert(strcmp(card.initials.ptr, "AB") == 0);
    assert(card.has_email);
    contacts_directory_Card_free(&card);

    // sorted([Contact]) -> [Contact], ordered by last name.
    contacts_contacts_Contact pair[2] = {zoe, ada};
    contacts_list_contacts_Contact in;
    in.items = pair;
    in.len = 2;
    memset(&w, 0, sizeof w);
    contacts_list_contacts_Contact_write(&w, &in);
    buf = contacts_directory_sorted(w.ptr, w.len, &len, &err);
    contacts_writer_free(&w);
    assert(err.code == 0);
    contacts_list_contacts_Contact sorted;
    assert(contacts_list_contacts_Contact_decode(buf, len, &sorted));
    contacts_free_bytes((uint8_t*)buf, len);
    assert(sorted.len == 2);
    assert(strcmp(sorted.items[0].last_name.ptr, "Byron") == 0);
    assert(sorted.items[0].email != NULL);
    assert(strcmp(sorted.items[1].last_name.ptr, "Quinn") == 0);
    assert(sorted.items[1].email == NULL);
    contacts_list_contacts_Contact_free(&sorted);

    // An empty list reports the directory's own Empty code.
    in.len = 0;
    memset(&w, 0, sizeof w);
    contacts_list_contacts_Contact_write(&w, &in);
    assert(contacts_directory_sorted(w.ptr, w.len, &len, &err) == NULL);
    contacts_writer_free(&w);
    assert(err.code == contacts_directory_DirectoryError_Empty);
    assert(strcmp(err.message, "no contacts to list") == 0);
    contacts_error_clear(&err);

    contacts_contacts_Contact_free(&zoe);
    contacts_contacts_Contact_free(&ada);
}

int main(void) {
    check_contract();
    book_surface();
    nested_module();
    sibling_root();
    ASSERT_NO_LEAKS(contacts_debug_live);
    printf("c/contacts: OK\n");
    return 0;
}
