# frozen_string_literal: true
# Conformance consumer: events sample, Ruby target.
#
# Exercises the FFI::Function listener trampoline (register -> fire
# synchronously on send -> unregister stops delivery) and the opaque-iterator
# ABI behind `get_messages`. The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "events"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

received = []
sub = WeaveFFI.register_message_listener { |message| received << message }
expect(sub.positive?, "listener id positive (got #{sub})")

WeaveFFI.send_message("alpha")
WeaveFFI.send_message("beta")
expect(received == %w[alpha beta], "listener received sends (got #{received})")

msgs = WeaveFFI.get_messages.to_a
expect(msgs == %w[alpha beta], "iterator yields messages in order (got #{msgs})")

# Unregister stops delivery; the producer still records the message.
WeaveFFI.unregister_message_listener(sub)
WeaveFFI.send_message("gamma")
expect(received == %w[alpha beta], "no delivery after unregister (got #{received})")
expect(WeaveFFI.get_messages.to_a == %w[alpha beta gamma], "producer kept recording")

puts "ruby/events: OK"
