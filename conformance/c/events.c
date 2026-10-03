// Conformance consumer: events sample, C target (ABI revision 3).
//
// Exercises the raw callback-interface ABI: a hand-written `Subscriber`
// vtable (one function per method taking `void* ctx` first and a trailing
// `events_error* out_err`, plus the `free` entry the producer calls when it
// drops its last reference), the reference-counted `EventBus` object
// (`_clone`/`_destroy`, an object handed *to* the consumer through
// `on_attached`), `Delivery` return values steering `publish`'s accepted
// count, a consumer failure reported through `out_err` from `route` (the
// method whose Rust signature returns `Result<_, ForeignError>`) surfacing
// to the caller as code -4, the `Message` record decoded from a borrowed
// value buffer, the `messages()` iterator yielding owned strings, the
// `last_message()` optional, and the async `publish_later` launcher, whose
// subscriber callbacks run on a producer thread. Ends by asserting the
// producer's leak counters are zero.

#include "harness.h"

#include <stdatomic.h>

#include "events_buffer.h"

// ── consumer-side subscriber ───────────────────────────────────────────────

// Per-subscriber state the producer sees only as an opaque `void* ctx`.
typedef struct {
    const char* skip_topic;  // route() answers Skip for this topic
    const char* stop_topic;  // route() answers AcceptAndStop for this topic
    const char* fail_topic;  // route() reports a consumer failure
    int keep_bus;            // on_attached keeps the bus reference in `bus`
    int64_t received;        // running count returned from on_message
    int attached;            // on_attached invocations
    int routed;              // route invocations
    events_events_EventBus* bus;  // kept reference (when keep_bus)
    int64_t attached_count;  // subscriber_count observed inside on_attached
    // Fields of the last decoded Message.
    int64_t last_seq;
    char last_topic[64];
    char last_text[64];
    size_t last_tag_count;
    char last_tag0[64];
} sub_ctx;

static atomic_int g_freed = 0;

static int topic_is(const uint8_t* ptr, size_t len, const char* topic) {
    return topic != NULL && bytes_eq(ptr, len, topic);
}

static events_events_Delivery sub_route(void* ctx, const uint8_t* topic_ptr, size_t topic_len,
                                        events_error* out_err) {
    sub_ctx* s = (sub_ctx*)ctx;
    s->routed++;
    if (topic_is(topic_ptr, topic_len, s->fail_topic)) {
        events_error_set(out_err, -4, "subscriber rejected topic");
        return events_events_Delivery_Skip;
    }
    if (topic_is(topic_ptr, topic_len, s->skip_topic)) {
        return events_events_Delivery_Skip;
    }
    if (topic_is(topic_ptr, topic_len, s->stop_topic)) {
        return events_events_Delivery_AcceptAndStop;
    }
    return events_events_Delivery_Accept;
}

// The Message buffer is borrowed for the duration of the call.
static int64_t sub_on_message(void* ctx, const uint8_t* message_ptr, size_t message_len,
                              events_error* out_err) {
    (void)out_err;
    sub_ctx* s = (sub_ctx*)ctx;
    events_events_Message m;
    assert(events_events_Message_decode(message_ptr, message_len, &m));
    s->last_seq = m.seq;
    snprintf(s->last_topic, sizeof s->last_topic, "%s", m.topic.ptr);
    snprintf(s->last_text, sizeof s->last_text, "%s", m.text.ptr);
    s->last_tag_count = m.tags.len;
    s->last_tag0[0] = '\0';
    if (m.tags.len > 0) snprintf(s->last_tag0, sizeof s->last_tag0, "%s", m.tags.items[0].ptr);
    events_events_Message_free(&m);
    s->received++;
    return s->received;
}

// The bus arrives as one strong reference the consumer adopts: it is usable
// right here, and it is ours to keep or release.
static void sub_on_attached(void* ctx, events_events_EventBus* bus, events_error* out_err) {
    (void)out_err;
    sub_ctx* s = (sub_ctx*)ctx;
    s->attached++;
    events_error err = {0};
    s->attached_count = events_events_EventBus_subscriber_count(bus, &err);
    assert(err.code == 0);
    if (s->keep_bus) {
        s->bus = bus;
    } else {
        events_events_EventBus_destroy(bus);
    }
}

static void sub_free(void* ctx) {
    sub_ctx* s = (sub_ctx*)ctx;
    events_events_EventBus_destroy(s->bus);  // null is a no-op
    free(s);
    atomic_fetch_add(&g_freed, 1);
}

static const events_events_Subscriber_vtable SUB_VTABLE = {
    sub_route,
    sub_on_message,
    sub_on_attached,
    sub_free,
};

static sub_ctx* new_sub(const char* skip, const char* stop, const char* fail, int keep_bus) {
    sub_ctx* s = (sub_ctx*)calloc(1, sizeof *s);
    assert(s != NULL);
    s->skip_topic = skip;
    s->stop_topic = stop;
    s->fail_topic = fail;
    s->keep_bus = keep_bus;
    return s;
}

// ── helpers ────────────────────────────────────────────────────────────────

// publish(topic, text, tags): `tags` is a buffered `[string]`.
static int64_t publish(events_events_EventBus* bus, const char* topic, const char* text,
                       const char** tags, size_t ntags, events_error* err) {
    events_str views[4];
    assert(ntags <= 4);
    for (size_t i = 0; i < ntags; i++) views[i] = events_str_of(tags[i]);
    events_list_string list;
    list.items = views;
    list.len = ntags;
    events_writer w;
    memset(&w, 0, sizeof w);
    events_list_string_write(&w, &list);
    int64_t n = events_events_EventBus_publish(bus, STR(topic), STR(text), w.ptr, w.len, err);
    events_writer_free(&w);
    return n;
}

// Collect messages() into `out`, returning how many were yielded. Each item
// is an owned (ptr, len) string released with events_free_bytes.
static int collect_messages(events_events_EventBus* bus, char** out, int cap) {
    events_error err = {0};
    events_events_EventBus_MessagesIterator* it = events_events_EventBus_messages(bus, &err);
    assert(err.code == 0 && it != NULL);
    int n = 0;
    const uint8_t* item = NULL;
    size_t len = 0;
    while (events_events_EventBus_MessagesIterator_next(it, &item, &len, &err) == 1) {
        assert(err.code == 0 && n < cap);
        out[n] = (char*)calloc(len + 1, 1);
        memcpy(out[n++], item, len);
        events_free_bytes((uint8_t*)item, len);
    }
    assert(err.code == 0);
    events_events_EventBus_MessagesIterator_destroy(it);
    return n;
}

static void free_texts(char** texts, int n) {
    for (int i = 0; i < n; i++) free(texts[i]);
}

// Decode last_message()'s buffered `Message?`.
static events_events_Message* last_message(events_events_EventBus* bus) {
    events_error err = {0};
    size_t len = 0;
    const uint8_t* buf = events_events_EventBus_last_message(bus, &len, &err);
    assert(err.code == 0 && buf != NULL);
    events_events_Message* m = NULL;
    assert(events_opt_events_Message_decode(buf, len, &m));
    events_free_bytes((uint8_t*)buf, len);
    return m;
}

// ── async completion state ─────────────────────────────────────────────────
static atomic_int g_later_done = 0;
static int32_t g_later_err = -1;
static int64_t g_later_result = -1;

static void on_publish_later(void* context, events_error* err, int64_t result) {
    assert(context == (void*)0x1234);
    g_later_err = err ? err->code : 0;
    events_error_free(err);
    g_later_result = result;
    atomic_store(&g_later_done, 1);
}

int main(void) {
    events_error err = {0};

    assert(EVENTS_ABI_VERSION == 3u && events_abi_version() == EVENTS_ABI_VERSION);
    assert(events_events_checksum() == EVENTS_EVENTS_CHECKSUM);

    events_events_EventBus* bus = events_events_EventBus_new(&err);
    assert(err.code == 0 && bus != NULL);
    assert(events_events_EventBus_subscriber_count(bus, &err) == 0);

    // last_message() on an empty bus: an absent optional.
    assert(last_message(bus) == NULL);

    // messages() on an empty bus yields nothing.
    char* texts[8];
    assert(collect_messages(bus, texts, 8) == 0);

    // Three subscribers: `a` skips "quiet", stops on "stop", and keeps the
    // bus reference handed to on_attached; `b` accepts everything and drops
    // its bus reference; `c` fails on "boom".
    sub_ctx* a = new_sub("quiet", "stop", NULL, 1);
    sub_ctx* b = new_sub(NULL, NULL, NULL, 0);
    sub_ctx* c = new_sub(NULL, NULL, "boom", 0);

    assert(events_events_EventBus_subscribe(bus, a, &SUB_VTABLE, &err) == 1);
    assert(err.code == 0);
    assert(a->attached == 1);
    assert(a->attached_count == 0 && "on_attached runs before the bus retains us");
    assert(a->bus == bus && "the object handed to the callback is the same bus");
    assert(events_events_EventBus_subscribe(bus, b, &SUB_VTABLE, &err) == 2);
    assert(b->attached == 1 && b->attached_count == 1);
    assert(events_events_EventBus_subscribe(bus, c, &SUB_VTABLE, &err) == 3);
    assert(c->attached == 1 && c->attached_count == 2);
    assert(events_events_EventBus_subscriber_count(bus, &err) == 3);

    // The kept reference is usable independently of the original pointer.
    assert(events_events_EventBus_subscriber_count(a->bus, &err) == 3);

    // Everyone accepts "news": 3 deliveries, and each saw the same Message.
    const char* tags[] = {"x", "y"};
    assert(publish(bus, "news", "hello", tags, 2, &err) == 3);
    assert(err.code == 0);
    assert(a->received == 1 && b->received == 1 && c->received == 1);
    assert(a->last_seq == 1);
    assert(strcmp(a->last_topic, "news") == 0);
    assert(strcmp(a->last_text, "hello") == 0);
    assert(a->last_tag_count == 2 && strcmp(a->last_tag0, "x") == 0);
    assert(b->last_seq == 1 && strcmp(c->last_text, "hello") == 0);

    // `a` skips "quiet": 2 deliveries, a's count unchanged.
    assert(publish(bus, "quiet", "psst", NULL, 0, &err) == 2);
    assert(a->received == 1 && b->received == 2 && c->received == 2);
    assert(b->last_seq == 2 && b->last_tag_count == 0);

    // `a` answers AcceptAndStop for "stop": exactly 1 delivery, later
    // subscribers are never asked.
    int b_routed = b->routed;
    assert(publish(bus, "stop", "last", NULL, 0, &err) == 1);
    assert(a->received == 2 && b->received == 2 && c->received == 2);
    assert(a->last_seq == 3 && strcmp(a->last_text, "last") == 0);
    assert(b->routed == b_routed);

    // `route` is the Result-returning callback method: `c` reports a failure
    // through out_err on "boom", the producer receives it as an Err and
    // chooses to fail the whole publish with code -4 and the message.
    publish(bus, "boom", "x", NULL, 0, &err);
    assert(err.code == -4);
    assert(err.message != NULL && strstr(err.message, "rejected topic") != NULL);
    events_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);
    assert(a->received == 3 && b->received == 3 && c->received == 2 &&
           "subscribers before the failure were delivered to");

    // The bus (and its subscribers) stay usable afterward.
    assert(publish(bus, "ok", "y", NULL, 0, &err) == 3);
    assert(err.code == 0);

    // Async publish: the launcher returns at once; the subscribers' callbacks
    // and then the completion run on a producer thread.
    events_events_EventBus_publish_later(bus, STR("later"), STR("z"), on_publish_later,
                                         (void*)0x1234);
    for (int i = 0; i < 5000 && !atomic_load(&g_later_done); i++) sleep_ms(1);
    assert(atomic_load(&g_later_done));
    assert(g_later_err == 0);
    assert(g_later_result == 3);
    assert(strcmp(b->last_text, "z") == 0);

    // messages(): every published text in order, including the aborted one
    // (the bus logs before it dispatches).
    int n = collect_messages(bus, texts, 8);
    const char* expected[] = {"hello", "psst", "last", "x", "y", "z"};
    assert(n == 6);
    for (int i = 0; i < n; i++) assert(strcmp(texts[i], expected[i]) == 0);
    free_texts(texts, n);

    // last_message(): present, with the async publish's fields.
    events_events_Message* last = last_message(bus);
    assert(last != NULL);
    assert(last->seq == 6);
    assert(strcmp(last->topic.ptr, "later") == 0);
    assert(strcmp(last->text.ptr, "z") == 0);
    assert(last->tags.len == 0 && "publish_later attaches no tags");
    events_opt_events_Message_free(&last);
    assert(last == NULL);

    // route_once: a free function taking the callback interface. The
    // producer does not retain it, so `free` runs before the call returns.
    assert(atomic_load(&g_freed) == 0);
    sub_ctx* d = new_sub("quiet", NULL, NULL, 0);
    assert(events_events_route_once(d, &SUB_VTABLE, STR("quiet"), &err) ==
           events_events_Delivery_Skip);
    assert(err.code == 0);
    assert(atomic_load(&g_freed) == 1 && "route_once released its subscriber");
    sub_ctx* e = new_sub(NULL, "stop", NULL, 0);
    assert(events_events_route_once(e, &SUB_VTABLE, STR("stop"), &err) ==
           events_events_Delivery_AcceptAndStop);
    assert(events_events_route_once(new_sub(NULL, NULL, NULL, 0), &SUB_VTABLE,
                                    STR("anything"), &err) == events_events_Delivery_Accept);
    assert(atomic_load(&g_freed) == 3);

    // A failure from the Result-returning route surfaces the same way, and
    // the subscriber is still released exactly once.
    sub_ctx* f = new_sub(NULL, NULL, "boom", 0);
    events_events_route_once(f, &SUB_VTABLE, STR("boom"), &err);
    assert(err.code == -4);
    assert(strstr(err.message, "rejected topic") != NULL);
    events_error_clear(&err);
    assert(atomic_load(&g_freed) == 4);

    // Release the bus reference `a` kept, then drop every subscriber: each
    // `free` entry runs exactly once.
    events_events_EventBus_destroy(a->bus);
    a->bus = NULL;
    assert(events_events_EventBus_subscriber_count(bus, &err) == 3);
    events_events_EventBus_clear_subscribers(bus, &err);
    assert(err.code == 0);
    assert(atomic_load(&g_freed) == 7 && "clear_subscribers freed a, b, and c");
    assert(events_events_EventBus_subscriber_count(bus, &err) == 0);
    assert(publish(bus, "empty", "nobody", NULL, 0, &err) == 0);

    // Reference counting: clone yields the same pointer; destroying the
    // original leaves the clone usable.
    events_events_EventBus* again = events_events_EventBus_clone(bus);
    assert(again == bus);
    events_events_EventBus_destroy(bus);
    assert(events_events_EventBus_subscriber_count(again, &err) == 0);
    assert(err.code == 0);
    n = collect_messages(again, texts, 8);
    assert(n == 7);
    free_texts(texts, n);
    events_events_EventBus_destroy(again);
    events_events_EventBus_destroy(NULL);
    assert(events_events_EventBus_clone(NULL) == NULL);

    // Destroying a bus releases its subscribers too.
    events_events_EventBus* bus2 = events_events_EventBus_new(&err);
    sub_ctx* g = new_sub(NULL, NULL, NULL, 0);
    assert(events_events_EventBus_subscribe(bus2, g, &SUB_VTABLE, &err) == 1);
    assert(publish(bus2, "t", "u", NULL, 0, &err) == 1);
    assert(g->received == 1);
    events_events_EventBus_destroy(bus2);
    assert(atomic_load(&g_freed) == 8 && "destroying the bus frees its subscriber");

    ASSERT_NO_LEAKS(events_debug_live);
    printf("c/events: OK\n");
    return 0;
}
