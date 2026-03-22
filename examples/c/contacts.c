#include <stdio.h>
#include <stdint.h>
#include <stddef.h>
#include "../../generated/c/weaveffi.h"

/* Contacts sample ABI — declarations that match samples/contacts/src/lib.rs */
typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;

weaveffi_handle_t weaveffi_contacts_create_contact(
    const char* first_name, const char* last_name, const char* email,
    int32_t contact_type, weaveffi_error* out_err);
weaveffi_contacts_Contact** weaveffi_contacts_list_contacts(
    size_t* out_len, weaveffi_error* out_err);
int32_t weaveffi_contacts_count_contacts(weaveffi_error* out_err);
int64_t weaveffi_contacts_Contact_get_id(const weaveffi_contacts_Contact* ptr);
const char* weaveffi_contacts_Contact_get_first_name(const weaveffi_contacts_Contact* ptr);
const char* weaveffi_contacts_Contact_get_last_name(const weaveffi_contacts_Contact* ptr);
const char* weaveffi_contacts_Contact_get_email(const weaveffi_contacts_Contact* ptr);
int32_t weaveffi_contacts_Contact_get_contact_type(const weaveffi_contacts_Contact* ptr);
void weaveffi_contacts_Contact_free(weaveffi_contacts_Contact* ptr);
void weaveffi_contacts_Contact_list_free(weaveffi_contacts_Contact** list, size_t len);

static const char* type_label(int32_t ct) {
    switch (ct) {
    case 0: return "Personal";
    case 1: return "Work";
    case 2: return "Other";
    default: return "Unknown";
    }
}

int main(void) {
    weaveffi_error err = {0};

    weaveffi_handle_t h1 = weaveffi_contacts_create_contact(
        "Alice", "Smith", "alice@example.com", 0, &err);
    if (err.code) { printf("error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("Created contact #%llu\n", (unsigned long long)h1);

    weaveffi_handle_t h2 = weaveffi_contacts_create_contact(
        "Bob", "Jones", NULL, 1, &err);
    if (err.code) { printf("error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("Created contact #%llu\n", (unsigned long long)h2);

    int32_t count = weaveffi_contacts_count_contacts(&err);
    if (err.code) { printf("error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("\nTotal: %d contacts\n\n", count);

    size_t len = 0;
    weaveffi_contacts_Contact** list = weaveffi_contacts_list_contacts(&len, &err);
    if (err.code) { printf("error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }

    for (size_t i = 0; i < len; i++) {
        int64_t id = weaveffi_contacts_Contact_get_id(list[i]);
        const char* first = weaveffi_contacts_Contact_get_first_name(list[i]);
        const char* last = weaveffi_contacts_Contact_get_last_name(list[i]);
        const char* email = weaveffi_contacts_Contact_get_email(list[i]);
        int32_t ct = weaveffi_contacts_Contact_get_contact_type(list[i]);

        printf("  [%lld] %s %s", (long long)id, first, last);
        if (email) printf(" <%s>", email);
        printf(" (%s)\n", type_label(ct));

        weaveffi_free_string(first);
        weaveffi_free_string(last);
        if (email) weaveffi_free_string(email);
    }

    weaveffi_contacts_Contact_list_free(list, len);
    return 0;
}
