// Contacts C++ example.
//
// Demonstrates:
//   * Creating contacts via the generated `weaveffi::contacts_*` wrappers.
//   * Enumerating contacts as a `std::vector<weaveffi::Contact>`.
//   * RAII cleanup: each `weaveffi::Contact` owns its native handle and calls
//     `weaveffi_contacts_Contact_destroy` from its destructor when it goes
//     out of scope (or when the containing vector is destroyed).

#include <cstdint>
#include <iostream>
#include <optional>
#include <string>
#include <vector>

#include "weaveffi.hpp"

static const char* type_label(weaveffi::ContactType t) {
    switch (t) {
        case weaveffi::ContactType::Personal:
            return "Personal";
        case weaveffi::ContactType::Work:
            return "Work";
        case weaveffi::ContactType::Other:
            return "Other";
    }
    return "Unknown";
}

int main() {
    std::cout << "=== C++ Contacts Example ===\n\n";

    try {
        void* h1 = weaveffi::contacts_create_contact(
            "Alice", "Smith",
            std::optional<std::string>("alice@example.com"),
            weaveffi::ContactType::Personal);
        std::cout << "Created contact #" << reinterpret_cast<std::uintptr_t>(h1) << "\n";

        void* h2 = weaveffi::contacts_create_contact(
            "Bob", "Jones",
            std::nullopt,
            weaveffi::ContactType::Work);
        std::cout << "Created contact #" << reinterpret_cast<std::uintptr_t>(h2) << "\n";

        std::cout << "\nTotal: " << weaveffi::contacts_count_contacts() << " contacts\n\n";

        {
            // `list` owns a std::vector<Contact>. Every Contact destructor fires
            // when the vector goes out of scope at the closing brace.
            auto list = weaveffi::contacts_list_contacts();
            for (const auto& c : list) {
                std::cout << "  [" << c.id() << "] "
                          << c.first_name() << " " << c.last_name();
                if (auto e = c.email()) {
                    std::cout << " <" << *e << ">";
                }
                std::cout << " (" << type_label(c.contact_type()) << ")\n";
            }
        }

        // Fetch a single contact by handle. The returned Contact is move-only
        // and cleans up on scope exit.
        {
            auto fetched = weaveffi::contacts_get_contact(h1);
            std::cout << "\nFetched: " << fetched.first_name() << " "
                      << fetched.last_name() << "\n";
        }

        bool deleted = weaveffi::contacts_delete_contact(h2);
        std::cout << "Deleted contact #" << reinterpret_cast<std::uintptr_t>(h2)
                  << ": " << (deleted ? "true" : "false") << "\n";
        std::cout << "Remaining: " << weaveffi::contacts_count_contacts()
                  << " contact(s)\n";
    } catch (const weaveffi::WeaveFFIError& e) {
        std::cerr << "WeaveFFI error " << e.code() << ": " << e.what() << "\n";
        return 1;
    }

    return 0;
}
