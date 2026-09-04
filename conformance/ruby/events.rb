# frozen_string_literal: true
# Conformance consumer: events sample, Ruby target.
#
# Drives the ABI 2 callback-interface surface: a Ruby class that includes the
# generated `WeaveFFI::Subscriber` mixin and overrides its three methods, a
# reference-counted `EventBus` object (constructor, `dup`, `close`, use after
# close), the bus handing a strong `EventBus` reference to the consumer
# through `on_attached`, `Delivery` return values steering `publish`'s
# accepted count, a Ruby exception inside a callback surfacing to the caller
# as `WeaveFFI::Error` with FOREIGN_ERROR_CODE (-4) without crashing the VM,
# the blocking async `publish_later` bridge, the iterator-backed `messages`
# Enumerator, the optional `last_message` record, and the release of every
# consumer implementation (the producer's `free` entry drops the registry
# slot). The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "events"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# The registry of live callback implementations is private generated state,
# but its size is the only observable proof that the producer's `free` entry
# ran (and that a consumer implementation is not leaked or freed twice).
def live_callbacks
  WeaveFFI.instance_variable_get(:@wv_cb_registry).size
end

# A subscriber that records every call so the arguments can be asserted.
class Recorder
  include WeaveFFI::Subscriber

  attr_reader :routed, :messages, :attached

  def initialize(skip: nil, stop: nil, fail_on: nil, fail_in_message: nil)
    @skip = skip
    @stop = stop
    @fail_on = fail_on
    @fail_in_message = fail_in_message
    @routed = []
    @messages = []
    @attached = []
  end

  def route(topic)
    @routed << topic
    raise "boom: #{topic}" if topic == @fail_on
    return WeaveFFI::Delivery::SKIP if topic == @skip
    return WeaveFFI::Delivery::ACCEPT_AND_STOP if topic == @stop

    WeaveFFI::Delivery::ACCEPT
  end

  def on_message(message)
    raise ArgumentError, "on_message rejected #{message.text}" if message.text == @fail_in_message

    @messages << message
    @messages.length
  end

  def on_attached(bus)
    @attached << bus
  end
end

# The mixin supplies NotImplementedError defaults for anything not overridden.
class Bare
  include WeaveFFI::Subscriber
end

expect(WeaveFFI::FOREIGN_ERROR_CODE == -4, "FOREIGN_ERROR_CODE is -4")
expect(WeaveFFI::ABI_VERSION == 2, "bindings target ABI revision 2")
expect(WeaveFFI::Delivery::ACCEPT.zero?, "Delivery::ACCEPT == 0")
expect(WeaveFFI::Delivery::SKIP == 1, "Delivery::SKIP == 1")
expect(WeaveFFI::Delivery::ACCEPT_AND_STOP == 2, "Delivery::ACCEPT_AND_STOP == 2")

baseline = live_callbacks

bus = WeaveFFI::EventBus.new
expect(bus.is_a?(WeaveFFI::EventBus), "constructor returns an EventBus")
expect(!bus.closed?, "fresh bus is open")
expect(bus.subscriber_count.zero?, "fresh bus has no subscribers")
expect(bus.last_message.nil?, "last_message is nil before any publish")
expect(bus.messages.is_a?(Enumerator), "messages returns an Enumerator")
expect(bus.messages.to_a == [], "no messages before any publish")

# subscribe: the bus calls on_attached synchronously with a strong reference
# to itself, which the consumer adopts as a fresh wrapper to the same object.
first = Recorder.new(skip: "quiet", stop: "stop")
expect(bus.subscribe(first) == 1, "first subscribe returns count 1")
expect(first.attached.length == 1, "on_attached called once (got #{first.attached.length})")
handed = first.attached[0]
expect(handed.is_a?(WeaveFFI::EventBus), "on_attached receives an EventBus wrapper")
expect(handed.handle.address == bus.handle.address, "handed bus is the same underlying object")
expect(handed.subscriber_count == 1, "handed bus is usable (got #{handed.subscriber_count})")
handed.close
handed.close
expect(handed.closed?, "handed bus wrapper closed (idempotently)")
expect(bus.subscriber_count == 1, "original bus survives closing the handed reference")

second = Recorder.new
expect(bus.subscribe(second) == 2, "second subscribe returns count 2")
expect(second.attached.length == 1, "second on_attached called once")
second.attached[0].close
expect(live_callbacks == baseline + 2, "two live implementations registered")

# publish: Accept from both subscribers -> 2 accepted; the Message record
# arrives fully decoded (seq, topic, text, tags).
expect(bus.publish("news", "hello", %w[x y]) == 2, "both subscribers accept news")
expect(first.routed == %w[news], "first.route saw news (got #{first.routed})")
expect(second.routed == %w[news], "second.route saw news (got #{second.routed})")
expect(first.messages.length == 1 && second.messages.length == 1, "each got one message")
msg = first.messages[0]
expect(msg.is_a?(WeaveFFI::Message), "on_message receives a Message")
expect(msg.seq == 1, "seq starts at 1 (got #{msg.seq})")
expect(msg.topic == "news", "message topic (got #{msg.topic.inspect})")
expect(msg.text == "hello", "message text (got #{msg.text.inspect})")
expect(msg.tags == %w[x y], "message tags (got #{msg.tags})")
expect(second.messages[0] == msg, "both subscribers saw the same message (structural equality)")

# Skip from the first subscriber -> only the second accepts.
expect(bus.publish("quiet", "psst", []) == 1, "skip lowers the accepted count")
expect(first.routed == %w[news quiet], "first.route saw quiet")
expect(first.messages.length == 1, "skipped subscriber got no message")
expect(second.messages.length == 2, "second subscriber got psst")
expect(second.messages[1].seq == 2 && second.messages[1].tags == [], "second message seq 2, no tags")

# AcceptAndStop from the first subscriber -> delivered to it, second not asked.
expect(bus.publish("stop", "last", ["t"]) == 1, "accept-and-stop halts delivery")
expect(first.messages.length == 2, "stopping subscriber still receives")
expect(first.messages[1].text == "last", "stopping subscriber got last")
expect(second.routed == %w[news quiet], "later subscriber not routed after stop (got #{second.routed})")
expect(second.messages.length == 2, "later subscriber not delivered after stop")

# Async publish: blocks the caller until the producer-thread completion fires;
# the subscriber callbacks run from that thread through the FFI dispatcher.
expect(bus.publish_later("later", "async") == 2, "publish_later resolves with the accepted count")
expect(first.messages.length == 3 && second.messages.length == 3, "async publish delivered to both")
expect(first.messages[2].topic == "later" && first.messages[2].tags == [], "async message decoded")

# Iterator: every text in order, lazily, with early termination releasing the
# producer iterator; and the optional last_message record.
expect(bus.messages.to_a == %w[hello psst last async], "messages iterator (got #{bus.messages.to_a})")
expect(bus.messages.first(2) == %w[hello psst], "early break through first(2)")
collected = []
bus.messages.each { |t| collected << t.upcase }
expect(collected == %w[HELLO PSST LAST ASYNC], "block iteration")
expect(bus.messages.each_with_index.to_a.last == ["async", 3], "Enumerator composes with each_with_index")
last = bus.last_message
expect(last == WeaveFFI::Message.new(seq: 4, topic: "later", text: "async", tags: []),
       "last_message (got seq=#{last.seq} topic=#{last.topic} text=#{last.text} tags=#{last.tags})")

# A Ruby exception in route surfaces to the publishing caller as the brand
# error with FOREIGN_ERROR_CODE and the exception text; the VM and the bus
# both keep working, and the subscriber stays attached.
failing = Recorder.new(fail_on: "boom", fail_in_message: "bad-body")
expect(bus.subscribe(failing) == 3, "third subscribe returns count 3")
failing.attached[0].close
begin
  bus.publish("boom", "x", [])
  raise "expected WeaveFFI::Error from a raising route"
rescue WeaveFFI::Error => e
  expect(e.code == WeaveFFI::FOREIGN_ERROR_CODE, "raising route -> code -4 (got #{e.code})")
  expect(e.message.include?("boom: boom"), "foreign error carries the exception text (got #{e.message.inspect})")
end
expect(failing.routed == %w[boom], "failing subscriber's route ran")
expect(first.messages.length == 4, "earlier subscribers were delivered before the failure")

# The same for an exception raised in on_message.
begin
  bus.publish("body", "bad-body", [])
  raise "expected WeaveFFI::Error from a raising on_message"
rescue WeaveFFI::Error => e
  expect(e.code == -4, "raising on_message -> code -4 (got #{e.code})")
  expect(e.message.include?("on_message rejected bad-body"), "on_message failure text (got #{e.message.inspect})")
end
expect(failing.messages.empty?, "raising on_message recorded nothing")

expect(bus.publish("ok", "y", []) == 3, "bus still usable after foreign errors")
expect(failing.messages.length == 1 && failing.messages[0].text == "y", "failing subscriber accepts ok")
expect(bus.subscriber_count == 3, "subscriber_count == 3")

# Non-ASCII text survives every string path: the C-string topic handed to
# route, the buffered Message, the iterator item, and the last_message record.
expect(bus.publish("тема ✓", "日本語", ["ü"]) == 3, "unicode publish accepted by all")
expect(first.routed.last == "тема ✓", "route received the unicode topic (got #{first.routed.last.inspect})")
expect(first.routed.last.encoding == Encoding::UTF_8, "route topic is UTF-8 (got #{first.routed.last.encoding})")
expect(first.messages.last.text == "日本語" && first.messages.last.tags == ["ü"], "unicode message fields")
expect(bus.messages.to_a.last == "日本語", "iterator item is unicode (got #{bus.messages.to_a.last.inspect})")
expect(bus.messages.to_a.last.encoding == Encoding::UTF_8, "iterator item is UTF-8")
expect(bus.last_message.topic == "тема ✓", "last_message topic is unicode")

# The producer logs a message before routing, so failed publishes are logged.
expect(bus.messages.to_a == ["hello", "psst", "last", "async", "x", "bad-body", "y", "日本語"],
       "log includes failed publishes (got #{bus.messages.to_a})")

# route_once: a free function taking the callback interface; the producer
# drops its only reference on return, so `free` runs and the slot is released.
before = live_callbacks
probe = Recorder.new(skip: "quiet")
expect(WeaveFFI.route_once(probe, "quiet") == WeaveFFI::Delivery::SKIP, "route_once -> SKIP")
expect(WeaveFFI.route_once(probe, "news") == WeaveFFI::Delivery::ACCEPT, "route_once -> ACCEPT")
expect(probe.routed == %w[quiet news], "route_once called route with the topic")
expect(probe.attached.empty?, "route_once never attaches")
expect(live_callbacks == before, "route_once released its implementation (#{live_callbacks} vs #{before})")

# A mixin default (NotImplementedError) is a foreign error too.
begin
  WeaveFFI.route_once(Bare.new, "x")
  raise "expected WeaveFFI::Error from the NotImplementedError default"
rescue WeaveFFI::Error => e
  expect(e.code == -4, "NotImplementedError default -> code -4 (got #{e.code})")
  expect(e.message.include?("route is not implemented"), "default message (got #{e.message.inspect})")
end
expect(live_callbacks == before, "failing route_once released its implementation")

# dup mints an independent wrapper (own strong reference) to the same bus.
twin = bus.dup
expect(twin.handle.address == bus.handle.address, "dup shares the object")
expect(twin.subscriber_count == 3, "dup sees the subscribers")
twin.close
expect(bus.subscriber_count == 3, "bus survives closing the dup")

# clear_subscribers drops every retained implementation; each `free` runs
# synchronously, and publishing to an empty bus accepts nothing.
bus.clear_subscribers
expect(bus.subscriber_count.zero?, "no subscribers after clear")
expect(live_callbacks == baseline, "every implementation released after clear (#{live_callbacks} vs #{baseline})")
expect(bus.publish("after", "nobody", []) == 0, "publish to an empty bus accepts 0")
expect(first.messages.length == 7, "cleared subscriber no longer delivered (got #{first.messages.length})")

# A subscriber held only by the bus is freed when the bus's last reference
# goes away. The reference handed through on_attached counts, so the bus
# outlives `short.close` until that wrapper is closed too.
short = WeaveFFI::EventBus.new
tail = Recorder.new
short.subscribe(tail)
expect(live_callbacks == baseline + 1, "bus retains its subscriber")
short.close
expect(live_callbacks == baseline + 1, "the on_attached reference keeps the bus (and subscriber) alive")
expect(tail.attached[0].subscriber_count == 1, "bus usable through the on_attached reference alone")
tail.attached[0].close
expect(live_callbacks == baseline, "dropping the last bus reference frees its subscriber")

# close is idempotent and a closed wrapper refuses further calls.
bus.close
bus.close
expect(bus.closed?, "closed? after close")
begin
  bus.subscriber_count
  raise "expected Error when using a closed EventBus"
rescue WeaveFFI::Error => e
  expect(e.message.include?("after close"), "use-after-close message (got #{e.message.inspect})")
end

# Ruby-side wrappers that were never closed release at GC without double
# frees; force a couple of collections to shake that out.
GC.start
GC.start

puts "ruby/events: OK"
