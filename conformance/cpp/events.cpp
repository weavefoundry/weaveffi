// Conformance consumer: events sample, C++ target.
//
// Drives the *generated* idiomatic wrapper. The 0.5.0 surface puts functions
// and listeners at bare snake_case names inside the per-module namespace
// (`weaveffi::events::send_message`, not `weaveffi::events_send_message`).
// Two key assertions:
//  - `events::get_messages()` returns a lazy single-pass range (RAII handle,
//    `begin()`/`end()`, one producer `next` per step): that only compiles and
//    runs if the wrapper uses the real opaque-iterator ABI
//    (launcher + next/destroy).
//  - the `std::function` listener wrapper round-trips: register pins the
//    closure, the producer fires it synchronously on send, unregister stops
//    delivery.

#include <cassert>
#include <cstdio>
#include <string>
#include <vector>

#include "weaveffi.hpp"

int main() {
    std::vector<std::string> received;
    uint64_t sub = weaveffi::events::register_message_listener(
        [&received](std::string message) { received.push_back(std::move(message)); });
    assert(sub > 0);

    weaveffi::events::send_message("alpha");
    weaveffi::events::send_message("beta");
    assert(received.size() == 2);
    assert(received[0] == "alpha");
    assert(received[1] == "beta");

    // The lazy range materializes through iteration: one producer `next` per
    // step, handle destroyed on exhaustion.
    std::vector<std::string> msgs;
    for (auto&& m : weaveffi::events::get_messages()) msgs.push_back(m);
    assert(msgs.size() == 2);
    assert(msgs[0] == "alpha");
    assert(msgs[1] == "beta");

    // Unregister stops delivery; the producer still records the message.
    weaveffi::events::unregister_message_listener(sub);
    weaveffi::events::send_message("gamma");
    assert(received.size() == 2);
    msgs.clear();
    for (auto&& m : weaveffi::events::get_messages()) msgs.push_back(m);
    assert(msgs.size() == 3);

    // Abandoning the range early destroys the handle via RAII.
    {
        auto range = weaveffi::events::get_messages();
        auto it = range.begin();
        assert(*it == "alpha");
    }

    std::printf("cpp/events: OK\n");
    return 0;
}
