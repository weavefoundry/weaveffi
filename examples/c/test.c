/*
 * End-to-end consumer test for the C binding consumer.
 *
 * Links against both libcalculator and libcontacts. Exercises a
 * representative slice of the C ABI: add, create_contact,
 * list_contacts, delete_contact. Prints "OK" and exits 0 on success;
 * any assertion failure prints a diagnostic and exits 1.
 */
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>
#include "../../generated/c/weaveffi.h"

typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;

int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);
weaveffi_handle_t weaveffi_contacts_create_contact(
    const char* first_name, const char* last_name, const char* email,
    int32_t contact_type, weaveffi_error* out_err);
weaveffi_contacts_Contact** weaveffi_contacts_list_contacts(
    size_t* out_len, weaveffi_error* out_err);
int64_t weaveffi_contacts_Contact_get_id(const weaveffi_contacts_Contact* ptr);
void weaveffi_contacts_Contact_list_free(weaveffi_contacts_Contact** list, size_t len);
int32_t weaveffi_contacts_delete_contact(weaveffi_handle_t id, weaveffi_error* out_err);
int32_t weaveffi_contacts_count_contacts(weaveffi_error* out_err);

#define ASSERT(cond, msg) do { \
    if (!(cond)) { \
        fprintf(stderr, "assertion failed: %s (%s:%d)\n", msg, __FILE__, __LINE__); \
        return 1; \
    } \
} while (0)

int main(void) {
    weaveffi_error err = {0};
    int32_t sum = weaveffi_calculator_add(2, 3, &err);
    ASSERT(err.code == 0, "calculator_add error");
    ASSERT(sum == 5, "calculator_add(2,3) != 5");

    err = (weaveffi_error){0};
    weaveffi_handle_t h = weaveffi_contacts_create_contact(
        "Alice", "Smith", "alice@example.com", 0, &err);
    ASSERT(err.code == 0, "create_contact error");
    ASSERT(h != 0, "create_contact returned 0");

    err = (weaveffi_error){0};
    size_t len = 0;
    weaveffi_contacts_Contact** items = weaveffi_contacts_list_contacts(&len, &err);
    ASSERT(err.code == 0, "list_contacts error");
    ASSERT(len == 1, "list_contacts length != 1");
    ASSERT(items != NULL, "list_contacts null");
    ASSERT(weaveffi_contacts_Contact_get_id(items[0]) == (int64_t)h, "id mismatch");
    weaveffi_contacts_Contact_list_free(items, len);

    err = (weaveffi_error){0};
    int32_t deleted = weaveffi_contacts_delete_contact(h, &err);
    ASSERT(err.code == 0, "delete_contact error");
    ASSERT(deleted == 1, "delete_contact did not return 1");

    err = (weaveffi_error){0};
    ASSERT(weaveffi_contacts_count_contacts(&err) == 0, "store not empty after cleanup");

    printf("OK\n");
    return 0;
}
