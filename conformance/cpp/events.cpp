// Conformance consumer: events sample, C++ target.
//
// Drives the *generated* idiomatic wrapper. The key assertion is that
// `events_get_messages()` returns a populated `std::vector<std::string>`: that
// only compiles and runs if the wrapper uses the real opaque-iterator ABI
// (launcher + next/destroy), which the pre-overhaul generator got wrong by
// lowering `iter<T>` as a list.

#include <cassert>
#include <cstdio>
#include <string>
#include <vector>

#include "weaveffi.hpp"

int main() {
    weaveffi::events_send_message("alpha");
    weaveffi::events_send_message("beta");
    weaveffi::events_send_message("gamma");

    std::vector<std::string> msgs = weaveffi::events_get_messages();
    assert(msgs.size() == 3);
    assert(msgs[0] == "alpha");
    assert(msgs[1] == "beta");
    assert(msgs[2] == "gamma");

    std::printf("cpp/events: OK\n");
    return 0;
}
