// Conformance consumer: events sample, C++ target (ABI revision 2).
//
// Drives the generated header-only wrapper over the callback-interface and
// reference-counted-object surface:
//  - `Subscriber` is an abstract class the consumer subclasses; a
//    `std::shared_ptr<Subscriber>` crosses into the producer, which calls
//    `route`, `on_message`, and `on_attached` through a static vtable and
//    releases the box (running the consumer's destructor) when it drops its
//    last reference.
//  - `EventBus` is a copyable RAII wrapper: copies `_clone` the producer
//    object and share it; moves transfer the handle; destructors release.
//  - the `bus` handed to `on_attached` is an owned wrapper the consumer may
//    keep and use.
//  - `Delivery` return values steer `publish`'s accepted count.
//  - a subscriber that throws surfaces to the caller as `WeaveFFIError`
//    with code -4 and leaves the bus usable.
//  - `messages()` is a lazy single-pass range; `last_message()` is an
//    optional record; `publish_later` is a std::future settled from a
//    producer thread.
// Exits non-zero on the first failed check.

#include <cstdio>
#include <cstdlib>
#include <future>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "weaveffi.hpp"

using namespace weaveffi;

static void check(bool ok, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "cpp/events: FAIL: %s\n", what);
        std::exit(1);
    }
}

// State outlives the subscriber so the test can inspect it after the
// producer has freed the implementation.
struct SubState {
    std::vector<std::string> routed;
    std::vector<Message> received;
    int attached = 0;
    int64_t attached_count_seen = -1;
    const weaveffi_events_EventBus* attached_handle = nullptr;
    std::optional<EventBus> kept_bus;
    int freed = 0;
};

class RecordingSubscriber : public Subscriber {
    std::shared_ptr<SubState> state_;
    std::string skip_topic_;
    std::string stop_topic_;
    bool keep_bus_;

public:
    RecordingSubscriber(std::shared_ptr<SubState> state, std::string skip_topic,
                        std::string stop_topic, bool keep_bus)
        : state_(std::move(state)),
          skip_topic_(std::move(skip_topic)),
          stop_topic_(std::move(stop_topic)),
          keep_bus_(keep_bus) {}

    ~RecordingSubscriber() override { state_->freed++; }

    Delivery route(const std::string& topic) override {
        state_->routed.push_back(topic);
        if (topic == skip_topic_) return Delivery::Skip;
        if (topic == stop_topic_) return Delivery::AcceptAndStop;
        return Delivery::Accept;
    }

    int64_t on_message(const Message& message) override {
        state_->received.push_back(message);
        return static_cast<int64_t>(state_->received.size());
    }

    void on_attached(EventBus bus) override {
        state_->attached++;
        state_->attached_handle = bus.handle();
        // The wrapper is fully usable inside the callback; `subscribe` calls
        // `on_attached` before it pushes the subscriber, so the count is the
        // pre-subscribe value.
        state_->attached_count_seen = bus.subscriber_count();
        if (keep_bus_) state_->kept_bus = std::move(bus);
        // Otherwise `bus` goes out of scope here and releases its reference.
    }
};

// Throws from `route` for one topic and from `on_message` for one text.
class ThrowingSubscriber : public Subscriber {
    std::string route_bomb_;
    std::string message_bomb_;

public:
    ThrowingSubscriber(std::string route_bomb, std::string message_bomb)
        : route_bomb_(std::move(route_bomb)), message_bomb_(std::move(message_bomb)) {}

    Delivery route(const std::string& topic) override {
        if (topic == route_bomb_) throw std::runtime_error("route rejected " + topic);
        return Delivery::Accept;
    }

    int64_t on_message(const Message& message) override {
        if (message.text == message_bomb_) throw std::logic_error("on_message exploded");
        return 1;
    }

    void on_attached(EventBus) override {}
};

int main() {
    check_abi_version();

    EventBus bus;
    check(bus.handle() != nullptr, "constructor yields a live handle");
    check(bus.subscriber_count() == 0, "fresh bus has no subscribers");
    check(!bus.last_message().has_value(), "fresh bus has no last message");
    {
        int n = 0;
        for (auto&& m : bus.messages()) {
            (void)m;
            n++;
        }
        check(n == 0, "fresh bus streams no messages");
    }

    // Copy semantics: a copy shares the object (same pointer), both usable.
    {
        EventBus copy = bus;
        check(copy.handle() == bus.handle(), "copy constructor clones the same object");
        check(copy.subscriber_count() == 0, "copy is usable");
        EventBus assigned;
        check(assigned.handle() != bus.handle(), "a second constructor makes a distinct object");
        assigned = bus;
        check(assigned.handle() == bus.handle(), "copy assignment clones the same object");
        EventBus& same = assigned;
        assigned = same;
        check(assigned.handle() == bus.handle(), "self-assignment is a no-op");
        EventBus moved = std::move(copy);
        check(moved.handle() == bus.handle(), "move constructor transfers the handle");
        check(copy.handle() == nullptr, "moved-from wrapper is empty");
        // All three destruct here: two releases plus a harmless empty one.
    }
    check(bus.subscriber_count() == 0, "bus survives its copies' destructors");

    // Subscribers: A skips "quiet", B stops on "stop", C accepts everything.
    auto a_state = std::make_shared<SubState>();
    auto b_state = std::make_shared<SubState>();
    auto c_state = std::make_shared<SubState>();
    check(bus.subscribe(std::make_shared<RecordingSubscriber>(a_state, "quiet", "", true)) == 1,
          "subscribe returns 1");
    check(a_state->attached == 1, "on_attached fired once for A");
    check(a_state->attached_handle == bus.handle(), "on_attached received the same bus object");
    check(a_state->attached_count_seen == 0, "on_attached ran before the subscriber was retained");
    check(a_state->kept_bus.has_value(), "A kept the bus handed to it");
    check(a_state->kept_bus->subscriber_count() == 1, "the kept bus is usable after the callback");

    check(bus.subscribe(std::make_shared<RecordingSubscriber>(b_state, "", "stop", false)) == 2,
          "subscribe returns 2");
    check(b_state->attached == 1 && b_state->attached_count_seen == 1, "B attached second");
    check(bus.subscribe(std::make_shared<RecordingSubscriber>(c_state, "", "", false)) == 3,
          "subscribe returns 3");
    check(bus.subscriber_count() == 3, "subscriber_count is 3");
    check(a_state->freed == 0 && b_state->freed == 0 && c_state->freed == 0,
          "retained subscribers are not freed");

    // Everyone accepts.
    check(bus.publish("news", "hello", {"a", "b"}) == 3, "publish delivers to all three");
    check(a_state->received.size() == 1 && b_state->received.size() == 1 &&
              c_state->received.size() == 1,
          "each subscriber received one message");
    {
        const Message& m = a_state->received[0];
        check(m.seq == 1, "first message has seq 1");
        check(m.topic == "news", "message topic");
        check(m.text == "hello", "message text");
        check(m.tags.size() == 2 && m.tags[0] == "a" && m.tags[1] == "b", "message tags");
    }
    check(a_state->routed.size() == 1 && a_state->routed[0] == "news", "route saw the topic");

    // A skips "quiet".
    check(bus.publish("quiet", "psst", {}) == 2, "Skip lowers the accepted count");
    check(a_state->received.size() == 1, "A was skipped");
    check(a_state->routed.size() == 2 && a_state->routed[1] == "quiet", "A was still asked");
    check(b_state->received.size() == 2 && b_state->received[1].seq == 2, "B got seq 2");
    check(c_state->received[1].tags.empty(), "empty tags list round-trips");

    // B stops the chain, so C is neither asked nor delivered.
    check(bus.publish("stop", "last", {"x"}) == 2, "AcceptAndStop halts later subscribers");
    check(a_state->received.size() == 2, "A accepted before the stop");
    check(b_state->received.size() == 3, "B accepted with stop");
    check(c_state->received.size() == 2, "C did not receive after the stop");
    check(c_state->routed.size() == 2, "C was not asked after the stop");

    // Iterator: one producer `next` per step, in publish order.
    {
        std::vector<std::string> texts;
        for (auto&& t : bus.messages()) texts.push_back(t);
        check(texts.size() == 3, "messages streams three texts");
        check(texts[0] == "hello" && texts[1] == "psst" && texts[2] == "last",
              "messages are in publish order");
    }
    // Abandoning the range early destroys the producer iterator via RAII.
    {
        auto range = bus.messages();
        auto it = range.begin();
        check(*it == "hello", "early-abandoned range yields the first element");
    }
    // Manual `next()` drains and then reports exhaustion.
    {
        auto range = bus.messages();
        int n = 0;
        while (range.next().has_value()) n++;
        check(n == 3, "manual next() yields three elements");
        check(!range.next().has_value(), "exhausted range stays exhausted");
    }

    // Optional record return.
    {
        std::optional<Message> last = bus.last_message();
        check(last.has_value(), "last_message is present");
        check(last->seq == 3 && last->topic == "stop" && last->text == "last",
              "last_message is the third message");
        check(last->tags.size() == 1 && last->tags[0] == "x", "last_message tags");
    }

    // Async: settled from a producer thread; subscribers run there too.
    {
        std::future<int64_t> pending = bus.publish_later("later", "async");
        check(pending.get() == 3, "publish_later resolves with the accepted count");
        check(a_state->received.size() == 3 && a_state->received[2].seq == 4,
              "A received the async message");
        check(b_state->received.size() == 4 && b_state->received[3].text == "async",
              "B received the async message");
        check(c_state->received.size() == 3 && c_state->received[2].topic == "later",
              "C received the async message");
        std::optional<Message> last = bus.last_message();
        check(last.has_value() && last->seq == 4 && last->tags.empty(),
              "async publish is logged with empty tags");
    }

    // Free function taking a callback interface: routes without retaining.
    {
        auto tmp_state = std::make_shared<SubState>();
        auto tmp = std::make_shared<RecordingSubscriber>(tmp_state, "quiet", "stop", false);
        std::weak_ptr<Subscriber> weak = tmp;
        check(events::route_once(std::move(tmp), "quiet") == Delivery::Skip, "route_once Skip");
        check(weak.expired() && tmp_state->freed == 1,
              "route_once released the subscriber when the call returned");
        tmp_state = std::make_shared<SubState>();
        check(events::route_once(std::make_shared<RecordingSubscriber>(tmp_state, "", "stop", false),
                                 "stop") == Delivery::AcceptAndStop,
              "route_once AcceptAndStop");
        check(events::route_once(std::make_shared<RecordingSubscriber>(tmp_state, "", "", false),
                                 "other") == Delivery::Accept,
              "route_once Accept");
        check(tmp_state->attached == 0, "route_once never calls on_attached");
    }

    // A null callback interface is rejected before crossing the ABI.
    {
        bool threw = false;
        try {
            bus.subscribe(nullptr);
        } catch (const std::invalid_argument&) {
            threw = true;
        }
        check(threw, "null subscriber throws std::invalid_argument");
        check(bus.subscriber_count() == 3, "null subscribe left the bus unchanged");
    }

    // Foreign errors: a throwing implementation aborts the call with -4.
    {
        EventBus bus2;
        auto ok_state = std::make_shared<SubState>();
        check(bus2.subscribe(std::make_shared<RecordingSubscriber>(ok_state, "", "", false)) == 1,
              "bus2 first subscriber");
        check(bus2.subscribe(std::make_shared<ThrowingSubscriber>("boom", "explode")) == 2,
              "bus2 throwing subscriber");

        bool caught = false;
        try {
            bus2.publish("boom", "x", {});
        } catch (const WeaveFFIError& e) {
            caught = true;
            check(e.code() == -4, "route exception maps to FOREIGN_ERROR_CODE (-4)");
            check(std::string(e.what()).find("route rejected boom") != std::string::npos,
                  "foreign error carries the C++ exception message");
        }
        check(caught, "publish threw for a throwing route");
        check(ok_state->received.size() == 1, "earlier subscriber was delivered before the abort");

        caught = false;
        try {
            bus2.publish("fine", "explode", {});
        } catch (const WeaveFFIError& e) {
            caught = true;
            check(e.code() == -4, "on_message exception maps to -4");
            check(std::string(e.what()).find("on_message exploded") != std::string::npos,
                  "on_message foreign error message");
        }
        check(caught, "publish threw for a throwing on_message");

        // The bus is still usable afterward, and the log kept both messages.
        check(bus2.publish("ok", "y", {}) == 2, "bus is usable after a foreign error");
        std::optional<Message> last = bus2.last_message();
        check(last.has_value() && last->seq == 3, "aborted publishes were still logged");

        // The async path surfaces the same code through the future.
        caught = false;
        try {
            bus2.publish_later("boom", "z").get();
        } catch (const WeaveFFIError& e) {
            caught = true;
            check(e.code() == -4, "async foreign error maps to -4");
        }
        check(caught, "publish_later future rethrows the foreign error");

        // route_once with a throwing subscriber.
        caught = false;
        try {
            events::route_once(std::make_shared<ThrowingSubscriber>("boom", ""), "boom");
        } catch (const WeaveFFIError& e) {
            caught = (e.code() == -4);
        }
        check(caught, "route_once surfaces the foreign error");

        bus2.clear_subscribers();
        check(ok_state->freed == 1, "clearing bus2 freed its subscribers");
    }

    // Releasing: clear_subscribers drops the producer's references, which
    // runs every implementation's destructor (A's releases its kept bus).
    bus.clear_subscribers();
    check(bus.subscriber_count() == 0, "clear_subscribers empties the bus");
    check(a_state->freed == 1 && b_state->freed == 1 && c_state->freed == 1,
          "free ran exactly once per subscriber");
    // The wrapper A stashed in its state still owns a reference and is
    // usable; dropping it releases that reference.
    check(a_state->kept_bus.has_value() && a_state->kept_bus->handle() == bus.handle(),
          "kept bus still points at the bus");
    check(a_state->kept_bus->subscriber_count() == 0, "kept bus is usable after free");
    a_state->kept_bus.reset();
    check(bus.publish("after", "clear", {}) == 0, "publishing with no subscribers accepts none");
    check(bus.last_message()->seq == 5, "log kept counting");

    // `bus` releases the last reference when main returns.
    std::printf("cpp/events: OK\n");
    return 0;
}
