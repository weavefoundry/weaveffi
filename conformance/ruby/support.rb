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

# The bindings' private runtime module (Bridge), which the consumers use
# for the leak counters and the contract check.
def bridge(mod)
  mod.const_get(:Bridge)
end

# The bindings' private module of attached C functions (Native), which the
# consumers use to send malformed input past the idiomatic wrappers.
def native(mod)
  mod.const_get(:Native)
end

# Runs the consumer body on its own thread, so none of its locals survive on
# a live stack once it returns, then forces collection (and the
# AutoPointer finalizers that release unclosed wrappers) and asserts the
# producer reports no live native resources: objects, callbacks, iterators,
# cancel tokens, or byte runs.
def run_and_check_leaks(mod)
  expect(bridge(mod).debug_live(-1) == 1, 'the library counts live resources')
  Thread.new { yield }.join
  kinds = %w[objects callbacks iterators tokens runs]
  live = nil
  100.times do
    GC.start(full_mark: true, immediate_sweep: true)
    live = (0..4).map { |k| bridge(mod).debug_live(k) }
    break if live.all?(&:zero?)

    sleep 0.02
  end
  report = kinds.zip(live).map { |k, n| "#{k}=#{n}" }.join(' ')
  expect(live.all?(&:zero?), "no live native resources at exit (#{report})")
end

# A minimal Fiber scheduler (Fiber.set_scheduler): enough for the blocking
# operations the bindings use while a call waits (Thread::Queue#pop and
# Mutex, through block/unblock, and sleep). It shows that an async call
# waiting in one non-blocking Fiber lets the thread's other Fibers run.
class MiniScheduler
  def initialize
    @ready = Thread::Queue.new
    @lock = Mutex.new
    @blocked = 0
    @sleeping = {}
  end

  def fiber(&block)
    fiber = Fiber.new(blocking: false, &block)
    fiber.resume
    fiber
  end

  def block(_blocker, _timeout = nil)
    @lock.synchronize { @blocked += 1 }
    Fiber.yield
  end

  # May be called from any thread.
  def unblock(_blocker, fiber)
    @ready << fiber
  end

  def kernel_sleep(duration = nil)
    @lock.synchronize { @sleeping[Fiber.current] = now + (duration || 0) }
    Fiber.yield
  end

  def io_wait(_io, _events, _timeout)
    raise NotImplementedError, 'MiniScheduler does no I/O'
  end

  # Runs every Fiber to completion (called when the thread's scheduler is
  # cleared, or the thread ends).
  def close
    loop do
      due = @lock.synchronize do
        ready = @sleeping.select { |_, at| at <= now }.keys
        ready.each { |f| @sleeping.delete(f) }
        ready
      end
      due.each { |f| f.resume if f.alive? }
      until @ready.empty?
        fiber = @ready.pop
        @lock.synchronize { @blocked -= 1 }
        fiber.resume if fiber.alive?
      end
      break if @lock.synchronize { @blocked.zero? && @sleeping.empty? }

      Thread.pass
      sleep(0.001)
    end
  end

  private

  def now
    Process.clock_gettime(Process::CLOCK_MONOTONIC)
  end
end
