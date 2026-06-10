// Conformance consumer: events sample, C++ target.
//
// Drives the *generated* idiomatic wrapper. Two key assertions:
//  - `events_get_messages()` returns a populated `std::vector<std::string>`:
//    that only compiles and runs if the wrapper uses the real opaque-iterator
//    ABI (launcher + next/destroy), which the pre-overhaul generator got wrong
//    by lowering `iter<T>` as a list.
//  - the `std::function` listener wrapper round-trips: register pins the
//    closure, the producer fires it synchronously on send, unregister stops
//    delivery (and intentionally leaks the shared_ptr box; see the header).

#include <cassert>
#include <cstdio>
#include <string>
#include <vector>

#include "weaveffi.hpp"

int main() {
    std::vector<std::string> received;
    uint64_t sub = weaveffi::events_register_message_listener(
        [&received](std::string message) { received.push_back(std::move(message)); });
    assert(sub > 0);

    weaveffi::events_send_message("alpha");
    weaveffi::events_send_message("beta");
    assert(received.size() == 2);
    assert(received[0] == "alpha");
    assert(received[1] == "beta");

    std::vector<std::string> msgs = weaveffi::events_get_messages();
    assert(msgs.size() == 2);
    assert(msgs[0] == "alpha");
    assert(msgs[1] == "beta");

    // Unregister stops delivery; the producer still records the message.
    weaveffi::events_unregister_message_listener(sub);
    weaveffi::events_send_message("gamma");
    assert(received.size() == 2);
    assert(weaveffi::events_get_messages().size() == 3);

    std::printf("cpp/events: OK\n");
    return 0;
}
