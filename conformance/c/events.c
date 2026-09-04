// Conformance consumer: events sample, C target (ABI revision 2).
//
// Exercises the raw callback-interface ABI: a hand-written `Subscriber`
// vtable (one function per method taking `void* ctx` first and a trailing
// `weaveffi_error* out_err`, plus the `free` entry the producer calls when
// it drops its last reference), the reference-counted `EventBus` object
// (`_clone`/`_destroy`, an object handed *to* the consumer through
// `on_attached`), `Delivery` return values steering `publish`'s accepted
// count, a foreign error raised from a callback surfacing to the caller as
// code -4, the `Message` record decoded from a borrowed value buffer, the
// `messages()` iterator, the `last_message()` optional, and the async
// `publish_later` launcher. Exits 0 on success; aborts on any mismatch.

#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "weaveffi.h"
#include "wvbuf.h"

// ── consumer-side subscriber ───────────────────────────────────────────────

// Per-subscriber state the producer sees only as an opaque `void* ctx`.
typedef struct {
    const char* skip_topic;  // route() answers Skip for this topic
    const char* stop_topic;  // route() answers AcceptAndStop for this topic
    const char* fail_topic;  // route() reports a foreign error for this topic
    int keep_bus;            // on_attached keeps the bus reference in `bus`
    int64_t received;        // running count returned from on_message
    int attached;            // on_attached invocations
    weaveffi_events_EventBus* bus;  // kept reference (when keep_bus)
    int64_t attached_count;  // subscriber_count observed inside on_attached
    // Fields of the last decoded Message.
    int64_t last_seq;
    char last_topic[64];
    char last_text[64];
    uint32_t last_tag_count;
    char last_tag0[64];
} sub_ctx;

static int g_freed = 0;

static weaveffi_events_Delivery sub_route(void* ctx, const char* topic,
                                          weaveffi_error* out_err) {
    sub_ctx* s = (sub_ctx*)ctx;
    if (s->fail_topic && strcmp(topic, s->fail_topic) == 0) {
        weaveffi_error_set(out_err, -4, "subscriber rejected topic");
        return weaveffi_events_Delivery_Skip;
    }
    if (s->skip_topic && strcmp(topic, s->skip_topic) == 0) {
        return weaveffi_events_Delivery_Skip;
    }
    if (s->stop_topic && strcmp(topic, s->stop_topic) == 0) {
        return weaveffi_events_Delivery_AcceptAndStop;
    }
    return weaveffi_events_Delivery_Accept;
}

// Message record: seq (i64), topic (string), text (string), tags ([string]).
static int64_t sub_on_message(void* ctx, const uint8_t* message_ptr,
                              size_t message_len, weaveffi_error* out_err) {
    (void)out_err;
    sub_ctx* s = (sub_ctx*)ctx;
    wv_reader r;
    wv_r_init(&r, message_ptr, message_len);
    s->last_seq = wv_get_i64(&r);
    char* topic = wv_get_str(&r);
    snprintf(s->last_topic, sizeof s->last_topic, "%s", topic);
    free(topic);
    char* text = wv_get_str(&r);
    snprintf(s->last_text, sizeof s->last_text, "%s", text);
    free(text);
    s->last_tag_count = wv_get_u32(&r);
    s->last_tag0[0] = '\0';
    for (uint32_t i = 0; i < s->last_tag_count; i++) {
        char* tag = wv_get_str(&r);
        if (i == 0) snprintf(s->last_tag0, sizeof s->last_tag0, "%s", tag);
        free(tag);
    }
    wv_r_expect_end(&r);
    s->received++;
    return s->received;
}

// The bus arrives as one strong reference the consumer adopts: it is usable
// right here, and it is ours to keep or release.
static void sub_on_attached(void* ctx, weaveffi_events_EventBus* bus,
                            weaveffi_error* out_err) {
    (void)out_err;
    sub_ctx* s = (sub_ctx*)ctx;
    s->attached++;
    weaveffi_error err = {0};
    s->attached_count = weaveffi_events_EventBus_subscriber_count(bus, &err);
    assert(err.code == 0);
    if (s->keep_bus) {
        s->bus = bus;
    } else {
        weaveffi_events_EventBus_destroy(bus);
    }
}

static void sub_free(void* ctx) {
    sub_ctx* s = (sub_ctx*)ctx;
    weaveffi_events_EventBus_destroy(s->bus);  // null is a no-op
    free(s);
    g_freed++;
}

static const weaveffi_events_Subscriber_vtable SUB_VTABLE = {
    sub_route,
    sub_on_message,
    sub_on_attached,
    sub_free,
};

static sub_ctx* new_sub(const char* skip, const char* stop, const char* fail,
                        int keep_bus) {
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
static int64_t publish(weaveffi_events_EventBus* bus, const char* topic,
                       const char* text, const char** tags, uint32_t ntags,
                       weaveffi_error* err) {
    wv_writer w;
    wv_w_init(&w);
    wv_put_u32(&w, ntags);
    for (uint32_t i = 0; i < ntags; i++) wv_put_str(&w, tags[i]);
    int64_t n = weaveffi_events_EventBus_publish(bus, topic, text, w.buf, w.len, err);
    wv_w_free(&w);
    return n;
}

// Collect messages() into `out`, returning how many were yielded.
static int collect_messages(weaveffi_events_EventBus* bus, char** out, int cap) {
    weaveffi_error err = {0};
    weaveffi_events_EventBus_MessagesIterator* it =
        weaveffi_events_EventBus_messages(bus, &err);
    assert(err.code == 0 && it != NULL);
    int n = 0;
    const char* item = NULL;
    while (weaveffi_events_EventBus_MessagesIterator_next(it, &item, &err) != 0) {
        assert(err.code == 0 && item != NULL);
        assert(n < cap);
        out[n++] = strdup(item);
        weaveffi_free_string(item);
    }
    assert(err.code == 0);
    weaveffi_events_EventBus_MessagesIterator_destroy(it);
    return n;
}

// ── async completion state ─────────────────────────────────────────────────
static atomic_int g_later_done = 0;
static int32_t g_later_err = -1;
static int64_t g_later_result = -1;

static void on_publish_later(void* context, weaveffi_error* err, int64_t result) {
    assert(context == (void*)0x1234);
    g_later_err = err ? err->code : 0;
    weaveffi_error_free(err);
    g_later_result = result;
    atomic_store(&g_later_done, 1);
}

int main(void) {
    weaveffi_error err = {0};

    // ABI revision handshake.
    assert(WEAVEFFI_ABI_VERSION == 2u);
    assert(weaveffi_abi_version() == 2u);

    weaveffi_events_EventBus* bus = weaveffi_events_EventBus_new(&err);
    assert(err.code == 0 && bus != NULL);
    assert(weaveffi_events_EventBus_subscriber_count(bus, &err) == 0);

    // last_message() on an empty bus: a buffered `Message?` with flag 0.
    size_t out_len = 0;
    const uint8_t* last = weaveffi_events_EventBus_last_message(bus, &out_len, &err);
    assert(err.code == 0 && last != NULL);
    {
        wv_reader r;
        wv_r_init(&r, last, out_len);
        assert(wv_get_bool(&r) == 0 && "no message yet");
        wv_r_expect_end(&r);
    }
    weaveffi_free_bytes((uint8_t*)last, out_len);

    // messages() on an empty bus yields nothing.
    char* texts[8];
    assert(collect_messages(bus, texts, 8) == 0);

    // Three subscribers: `a` skips "quiet", stops on "stop", and keeps the
    // bus reference handed to on_attached; `b` accepts everything and drops
    // its bus reference; `c` fails on "boom".
    sub_ctx* a = new_sub("quiet", "stop", NULL, 1);
    sub_ctx* b = new_sub(NULL, NULL, NULL, 0);
    sub_ctx* c = new_sub(NULL, NULL, "boom", 0);

    assert(weaveffi_events_EventBus_subscribe(bus, a, &SUB_VTABLE, &err) == 1);
    assert(err.code == 0);
    assert(a->attached == 1);
    assert(a->attached_count == 0 && "on_attached runs before the bus retains us");
    assert(a->bus == bus && "the object handed to the callback is the same bus");
    assert(weaveffi_events_EventBus_subscribe(bus, b, &SUB_VTABLE, &err) == 2);
    assert(b->attached == 1 && b->attached_count == 1);
    assert(weaveffi_events_EventBus_subscribe(bus, c, &SUB_VTABLE, &err) == 3);
    assert(c->attached == 1 && c->attached_count == 2);
    assert(weaveffi_events_EventBus_subscriber_count(bus, &err) == 3);

    // The kept reference is usable independently of the original pointer.
    assert(weaveffi_events_EventBus_subscriber_count(a->bus, &err) == 3);

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
    // subscribers never see it.
    assert(publish(bus, "stop", "last", NULL, 0, &err) == 1);
    assert(a->received == 2 && b->received == 2 && c->received == 2);
    assert(a->last_seq == 3 && strcmp(a->last_text, "last") == 0);

    // `c` raises through out_err on "boom": the whole publish aborts with
    // FOREIGN_ERROR_CODE and the foreign message.
    publish(bus, "boom", "x", NULL, 0, &err);
    assert(err.code == -4);
    assert(err.message != NULL && strstr(err.message, "rejected topic") != NULL);
    weaveffi_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    // The bus (and its subscribers) stay usable afterward.
    assert(publish(bus, "ok", "y", NULL, 0, &err) == 3);
    assert(err.code == 0);

    // Async publish: the completion callback fires on a producer thread with
    // the accepted count.
    weaveffi_events_EventBus_publish_later_async(bus, "later", "z", on_publish_later,
                                                 (void*)0x1234);
    for (int i = 0; i < 5000 && !atomic_load(&g_later_done); i++) usleep(1000);
    assert(atomic_load(&g_later_done));
    assert(g_later_err == 0);
    assert(g_later_result == 3);

    // messages(): every published text in order, including the aborted one
    // (the bus logs before it dispatches).
    int n = collect_messages(bus, texts, 8);
    const char* expected[] = {"hello", "psst", "last", "x", "y", "z"};
    assert(n == 6);
    for (int i = 0; i < n; i++) {
        assert(strcmp(texts[i], expected[i]) == 0);
        free(texts[i]);
    }

    // last_message(): present, with the async publish's fields.
    last = weaveffi_events_EventBus_last_message(bus, &out_len, &err);
    assert(err.code == 0 && last != NULL);
    {
        wv_reader r;
        wv_r_init(&r, last, out_len);
        assert(wv_get_bool(&r) == 1);
        assert(wv_get_i64(&r) == 6);
        char* topic = wv_get_str(&r);
        assert(strcmp(topic, "later") == 0);
        free(topic);
        char* text = wv_get_str(&r);
        assert(strcmp(text, "z") == 0);
        free(text);
        assert(wv_get_u32(&r) == 0 && "publish_later attaches no tags");
        wv_r_expect_end(&r);
    }
    weaveffi_free_bytes((uint8_t*)last, out_len);

    // route_once: a free function taking the callback interface. The
    // producer does not retain it, so `free` runs before the call returns.
    sub_ctx* d = new_sub("quiet", NULL, NULL, 0);
    assert(g_freed == 0);
    assert(weaveffi_events_route_once(d, &SUB_VTABLE, "quiet", &err) ==
           weaveffi_events_Delivery_Skip);
    assert(err.code == 0);
    assert(g_freed == 1 && "route_once released its subscriber");
    sub_ctx* e = new_sub(NULL, "stop", NULL, 0);
    assert(weaveffi_events_route_once(e, &SUB_VTABLE, "stop", &err) ==
           weaveffi_events_Delivery_AcceptAndStop);
    assert(weaveffi_events_route_once(new_sub(NULL, NULL, NULL, 0), &SUB_VTABLE,
                                      "anything", &err) ==
           weaveffi_events_Delivery_Accept);
    assert(g_freed == 3);

    // A foreign error from route_once surfaces the same way.
    sub_ctx* f = new_sub(NULL, NULL, "boom", 0);
    weaveffi_events_route_once(f, &SUB_VTABLE, "boom", &err);
    assert(err.code == -4);
    weaveffi_error_clear(&err);
    assert(g_freed == 4);

    // Release the bus reference `a` kept, then drop every subscriber: each
    // `free` entry runs exactly once.
    weaveffi_events_EventBus_destroy(a->bus);
    a->bus = NULL;
    assert(weaveffi_events_EventBus_subscriber_count(bus, &err) == 3);
    weaveffi_events_EventBus_clear_subscribers(bus, &err);
    assert(err.code == 0);
    assert(g_freed == 7 && "clear_subscribers freed a, b, and c");
    assert(weaveffi_events_EventBus_subscriber_count(bus, &err) == 0);
    assert(publish(bus, "empty", "nobody", NULL, 0, &err) == 0);

    // Reference counting: clone yields the same pointer; destroying the
    // original leaves the clone usable.
    weaveffi_events_EventBus* again = weaveffi_events_EventBus_clone(bus);
    assert(again == bus);
    weaveffi_events_EventBus_destroy(bus);
    assert(weaveffi_events_EventBus_subscriber_count(again, &err) == 0);
    assert(err.code == 0);
    n = collect_messages(again, texts, 8);
    assert(n == 7);
    for (int i = 0; i < n; i++) free(texts[i]);
    weaveffi_events_EventBus_destroy(again);
    weaveffi_events_EventBus_destroy(NULL);
    assert(weaveffi_events_EventBus_clone(NULL) == NULL);

    // Destroying a bus releases its subscribers too.
    weaveffi_events_EventBus* bus2 = weaveffi_events_EventBus_new(&err);
    sub_ctx* g = new_sub(NULL, NULL, NULL, 0);
    assert(weaveffi_events_EventBus_subscribe(bus2, g, &SUB_VTABLE, &err) == 1);
    assert(publish(bus2, "t", "u", NULL, 0, &err) == 1);
    assert(g->received == 1);
    weaveffi_events_EventBus_destroy(bus2);
    assert(g_freed == 8 && "destroying the bus frees its subscriber");

    printf("c/events: OK\n");
    return 0;
}
