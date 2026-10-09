# frozen_string_literal: true

# Shared helpers for the Ruby conformance consumers.

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Runs the block, which must raise a `cls`, and returns the exception.
def expect_raise(cls, msg)
  yield
rescue cls => e
  e
else
  raise "assertion failed: #{msg}: expected #{cls}, nothing was raised"
end

# Polls the block (up to about two seconds) until it's truthy.
def eventually
  200.times do
    return true if yield

    sleep 0.01
  end
  false
end

# The 64-bit FNV-1a hash the contract tables use.
def fnv1a64(text)
  text.bytes.reduce(0xcbf29ce484222325) { |h, b| ((h ^ b) * 0x100000001b3) & 0xffff_ffff_ffff_ffff }
end

# Runs the consumer body on its own thread, so none of its locals survive on
# a live stack once it returns, then forces collection (and the
# AutoPointer finalizers that release unclosed wrappers) and asserts the
# producer reports no live native resources: objects, callbacks, iterators,
# cancel tokens, or byte runs.
def run_and_check_leaks(mod)
  expect(mod._wv_debug_live(-1) == 1, 'the library counts live resources')
  Thread.new { yield }.join
  kinds = %w[objects callbacks iterators tokens runs]
  live = nil
  100.times do
    GC.start(full_mark: true, immediate_sweep: true)
    live = (0..4).map { |k| mod._wv_debug_live(k) }
    break if live.all?(&:zero?)

    sleep 0.02
  end
  report = kinds.zip(live).map { |k, n| "#{k}=#{n}" }.join(' ')
  expect(live.all?(&:zero?), "no live native resources at exit (#{report})")
end
