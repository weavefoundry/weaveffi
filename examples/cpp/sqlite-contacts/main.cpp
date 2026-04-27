// SQLite Contacts C++ example.
//
// Demonstrates three features of the WeaveFFI C++ bindings together:
//
//   * Async: every CRUD entry point returns a `std::future<T>` via the
//     generated `weaveffi::contacts_*` wrappers. Awaiting is just `fut.get()`.
//   * Cancellation: `contacts_create_contact` takes a `std::stop_token`. We
//     drive it with `std::future::wait_for(timeout)` — if the future hasn't
//     resolved within the timeout, flip the `std::stop_source`, which the
//     C++ wrapper forwards to `weaveffi_cancel_token_cancel`. The Rust worker
//     polls the token and returns `ERR_CODE_CANCELLED`, which surfaces as a
//     `weaveffi::WeaveFFIError` on the future.
//   * Iterators: the sqlite-contacts cdylib exposes streaming
//     `ListContactsIterator` + `_next` / `_destroy`. We iterate it via a tiny
//     helper in `iter_contacts.{hpp,cpp}` and wrap each yielded handle in a
//     RAII `weaveffi::Contact`.

#include <chrono>
#include <cstdint>
#include <future>
#include <iostream>
#include <optional>
#include <stop_token>
#include <string>
#include <vector>

#include "iter_contacts.hpp"
#include "weaveffi.hpp"

static const char* status_label(weaveffi::Status s) {
    switch (s) {
        case weaveffi::Status::Active:
            return "Active";
        case weaveffi::Status::Archived:
            return "Archived";
    }
    return "Unknown";
}

static std::vector<weaveffi::Contact> list_all(std::optional<weaveffi::Status> filter) {
    int32_t raw_filter = 0;
    const int32_t* filter_ptr = nullptr;
    if (filter.has_value()) {
        raw_filter = static_cast<int32_t>(*filter);
        filter_ptr = &raw_filter;
    }
    auto handles = sqlite_contacts::list_all_handles(filter_ptr);
    std::vector<weaveffi::Contact> contacts;
    contacts.reserve(handles.size());
    for (void* h : handles) {
        contacts.emplace_back(h);
    }
    return contacts;
}

int main() {
    std::cout << "=== C++ SQLite Contacts Example ===\n\n";

    try {
        // ── 1. Async create via std::future::get() ───────────────────────
        auto alice =
            weaveffi::contacts_create_contact("Alice", std::string("alice@example.com"))
                .get();
        std::cout << "Created #" << alice.id() << " " << alice.name() << "\n";

        auto bob = weaveffi::contacts_create_contact("Bob", std::nullopt).get();
        std::cout << "Created #" << bob.id() << " " << bob.name() << "\n";

        // ── 2. Async find (optional return) ──────────────────────────────
        if (auto found = weaveffi::contacts_find_contact(alice.id()).get()) {
            std::cout << "\nFound #" << found->id() << ": " << found->name()
                      << " <" << found->email().value_or("-") << ">\n";
        }

        // ── 3. Async update ──────────────────────────────────────────────
        bool updated =
            weaveffi::contacts_update_contact(alice.id(), std::string("alice@new.com"))
                .get();
        std::cout << "Updated alice's email: " << (updated ? "true" : "false")
                  << "\n";

        // ── 4. Streaming iterator ────────────────────────────────────────
        std::cout << "\nIterating contacts:\n";
        for (const auto& c : list_all(std::nullopt)) {
            std::cout << "  [" << c.id() << "] " << c.name() << " <"
                      << c.email().value_or("-") << "> ("
                      << status_label(c.status()) << ")\n";
        }

        // ── 5. Async count with optional filter ──────────────────────────
        int64_t total = weaveffi::contacts_count_contacts(std::nullopt).get();
        int64_t active =
            weaveffi::contacts_count_contacts(weaveffi::Status::Active).get();
        std::cout << "\nTotal=" << total << " Active=" << active << "\n";

        // ── 6. Cancellation via wait_for(timeout) + stop_source ──────────
        //
        // The Rust worker polls the cancel token ~20 times at 5 ms intervals
        // (≈100 ms) before it touches SQLite, so a 20 ms wait is guaranteed
        // to time out and give us a chance to request_stop(). The stop
        // callback inside the generated wrapper then invokes
        // weaveffi_cancel_token_cancel, and the worker returns
        // ERR_CODE_CANCELLED (code = 2, message = "cancelled") on its next
        // poll, which reaches us as a WeaveFFIError on the future.
        std::cout << "\nCancelling a slow create via wait_for(20ms)...\n";
        std::stop_source stop_src;
        auto pending = weaveffi::contacts_create_contact(
            "slow-insert", std::nullopt, stop_src.get_token());

        if (pending.wait_for(std::chrono::milliseconds(20)) ==
            std::future_status::timeout) {
            stop_src.request_stop();
            try {
                (void)pending.get();
                std::cerr << "expected WeaveFFIError after cancel\n";
                return 1;
            } catch (const weaveffi::WeaveFFIError& e) {
                std::cout << "  cancelled: code=" << e.code()
                          << " message=\"" << e.what() << "\"\n";
            }
        } else {
            std::cerr << "expected wait_for() to time out before work completed\n";
            return 1;
        }

        // ── 7. Async delete (cleanup) ────────────────────────────────────
        bool deleted = weaveffi::contacts_delete_contact(bob.id()).get();
        std::cout << "\nDeleted bob: " << (deleted ? "true" : "false") << "\n";
        int64_t remaining = weaveffi::contacts_count_contacts(std::nullopt).get();
        std::cout << "Remaining: " << remaining << "\n";
    } catch (const weaveffi::WeaveFFIError& e) {
        std::cerr << "WeaveFFI error " << e.code() << ": " << e.what() << "\n";
        return 1;
    } catch (const std::exception& e) {
        std::cerr << "Error: " << e.what() << "\n";
        return 1;
    }

    return 0;
}
