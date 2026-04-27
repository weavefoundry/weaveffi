// Iterator bridge for the sqlite-contacts sample.
//
// The generated `weaveffi.hpp` materialises `iter<T>` returns into an array
// (`T** + size_t*`) per the C++ generator's cross-target contract. The
// sqlite-contacts cdylib, however, implements the *streaming* iterator C ABI
// (`ListContactsIterator*` + `_next` / `_destroy`). Those two shapes are not
// ABI-compatible, so this tiny TU talks to the real iterator functions
// directly and returns raw handles that main.cpp wraps in `weaveffi::Contact`
// RAII objects.
//
// Kept in its own header/source pair so main.cpp can still `#include
// "weaveffi.hpp"` without colliding extern "C" declarations for
// `weaveffi_contacts_list_contacts`.
#pragma once

#include <cstdint>
#include <vector>

namespace sqlite_contacts {

// Iterate the contacts table via the streaming C ABI and return the raw
// `weaveffi_contacts_Contact*` handles (exposed as `void*` so this header has
// no dependency on the generated C++ wrapper types). The caller takes
// ownership of every pointer and is responsible for destroying each one,
// typically by wrapping it in `weaveffi::Contact`.
//
// Pass `status_filter = nullptr` to iterate every row.
std::vector<void*> list_all_handles(const int32_t* status_filter);

} // namespace sqlite_contacts
