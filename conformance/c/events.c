// Conformance consumer: events sample, C target.
//
// Exercises a context-carrying callback, a register/unregister listener pair
// (returning a uint64_t subscription id), and an opaque iterator driven by the
// `int32_t next(iter, &out_item, &err)` contract — the three features whose ABI
// historically drifted between generators and hand-written consumers.

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"

static int g_count = 0;
static char g_last[64] = {0};

static void on_message(const char* message, void* context) {
    int* counter = (int*)context;
    (*counter)++;
    g_count = *counter;
    strncpy(g_last, message, sizeof(g_last) - 1);
}

int main(void) {
    weaveffi_error err = {0, NULL};

    int counter = 0;
    uint64_t id = weaveffi_events_register_message_listener(on_message, &counter);
    assert(id > 0);

    weaveffi_events_send_message("hello", &err);
    assert(err.code == 0);
    weaveffi_events_send_message("world", &err);
    assert(err.code == 0);
    assert(counter == 2);
    assert(g_count == 2);
    assert(strcmp(g_last, "world") == 0);

    // Iterate via the opaque-handle ABI.
    weaveffi_events_GetMessagesIterator* it = weaveffi_events_get_messages(&err);
    assert(err.code == 0 && it != NULL);
    const char* expected[] = {"hello", "world"};
    int i = 0;
    const char* item = NULL;
    while (weaveffi_events_GetMessagesIterator_next(it, &item, &err)) {
        assert(err.code == 0);
        assert(strcmp(item, expected[i]) == 0);
        weaveffi_free_string(item);
        i++;
    }
    assert(err.code == 0);
    assert(i == 2);
    weaveffi_events_GetMessagesIterator_destroy(it);

    // Unregister stops delivery.
    weaveffi_events_unregister_message_listener(id);
    weaveffi_events_send_message("again", &err);
    assert(counter == 2);

    printf("c/events: OK\n");
    return 0;
}
