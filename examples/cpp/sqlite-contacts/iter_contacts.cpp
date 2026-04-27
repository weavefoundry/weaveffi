// Deliberately does NOT include the generated weaveffi.hpp so we can declare
// `weaveffi_contacts_list_contacts` with its real iterator-returning signature.
// See iter_contacts.hpp for the rationale.
#include "iter_contacts.hpp"

#include <cstdint>
#include <stdexcept>
#include <string>

extern "C" {

typedef struct weaveffi_error {
    int32_t code;
    const char* message;
} weaveffi_error;

typedef int32_t weaveffi_contacts_Status;

struct weaveffi_contacts_Contact;
struct weaveffi_contacts_ListContactsIterator;

weaveffi_contacts_ListContactsIterator* weaveffi_contacts_list_contacts(
    const weaveffi_contacts_Status* status,
    weaveffi_error* out_err);

int32_t weaveffi_contacts_ListContactsIterator_next(
    weaveffi_contacts_ListContactsIterator* iter,
    weaveffi_contacts_Contact** out_item,
    weaveffi_error* out_err);

void weaveffi_contacts_ListContactsIterator_destroy(
    weaveffi_contacts_ListContactsIterator* iter);

void weaveffi_error_clear(weaveffi_error* err);

} // extern "C"

namespace sqlite_contacts {

static std::runtime_error make_error(const char* prefix, weaveffi_error& err) {
    std::string msg = err.message ? err.message : "unknown error";
    int32_t code = err.code;
    weaveffi_error_clear(&err);
    return std::runtime_error(
        std::string(prefix) + " failed (" + std::to_string(code) + "): " + msg);
}

std::vector<void*> list_all_handles(const int32_t* status_filter) {
    weaveffi_error err{};
    auto* iter = weaveffi_contacts_list_contacts(status_filter, &err);
    if (err.code != 0 || !iter) {
        throw make_error("list_contacts", err);
    }

    std::vector<void*> handles;
    while (true) {
        weaveffi_contacts_Contact* item = nullptr;
        int32_t has_item =
            weaveffi_contacts_ListContactsIterator_next(iter, &item, &err);
        if (err.code != 0) {
            weaveffi_contacts_ListContactsIterator_destroy(iter);
            throw make_error("iterator.next", err);
        }
        if (has_item == 0) {
            break;
        }
        handles.push_back(item);
    }
    weaveffi_contacts_ListContactsIterator_destroy(iter);
    return handles;
}

} // namespace sqlite_contacts
