# frozen_string_literal: true

# Shared helpers for the Ruby conformance consumers.

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Runs the consumer body on its own thread, so none of its locals survive on
# a live stack once it returns, then forces collection (and the
# AutoPointer finalizers that release unclosed wrappers) and asserts the
# producer reports no live native resources: objects, callbacks, iterators,
# cancel tokens, or returned byte allocations.
def run_and_check_leaks(mod)
  Thread.new { yield }.join
  kinds = %w[objects callbacks iterators tokens allocations]
  live = nil
  50.times do
    GC.start(full_mark: true, immediate_sweep: true)
    live = (0..4).map { |k| mod._wv_debug_live(k) }
    break if live.all?(&:zero?)

    sleep 0.02
  end
  report = kinds.zip(live).map { |k, n| "#{k}=#{n}" }.join(' ')
  expect(live.all?(&:zero?), "no live native resources at exit (#{report})")
end
