# frozen_string_literal: true
# Conformance consumer: async-demo sample, Ruby target.
#
# Drives the blocking async bridge end to end: `run_task` blocks the calling
# thread until the producer's completion callback fires and decodes the
# TaskResult value class from a value buffer, an empty name raises the typed
# TaskError::InvalidName subclass, `run_batch` round-trips a list of records
# through value buffers both ways, `run_n_tasks` returns a direct scalar
# through the callback (also from many threads at once), and the cancellable
# `wait` honors a CancelToken passed as `cancel:` (cancelled from another
# thread, or before the call) and an interrupted wait (Thread#raise), each
# raising AsyncDemo::Cancelled or the interruption long before the timeout.
# `active_callbacks` is back to zero once every task body has completed or
# been dropped. The gem is installed from the generated tree; the cdylib is
# selected through ASYNC_DEMO_LIBRARY.

require_relative 'support'
require 'async_demo'

# Polls the producer until no task body is in flight.
def settle_tasks
  100.times do
    return true if AsyncDemo.active_callbacks.zero?

    sleep 0.02
  end
  false
end

run_and_check_leaks(AsyncDemo) do
  expect(AsyncDemo::ABI_VERSION == 3, 'bindings target ABI revision 3')

  # Async record return: blocks until the worker-thread callback delivers the
  # encoded TaskResult.
  result = AsyncDemo.run_task('alpha')
  expect(result.id.positive?, 'run_task assigns an id')
  expect(result.value == 'completed: alpha', "run_task value (got #{result.value})")
  expect(result.success == true, 'run_task success flag')

  # Typed async error: the empty name settles with the InvalidName subclass.
  begin
    AsyncDemo.run_task('')
    expect(false, 'expected TaskError::InvalidName for empty name')
  rescue AsyncDemo::TaskError::InvalidName => e
    expect(e.code == 1, "InvalidName carries code 1 (got #{e.code})")
    expect(e.is_a?(AsyncDemo::TaskError), 'subclass of TaskError')
    expect(e.is_a?(AsyncDemo::Error), 'subclass of AsyncDemo::Error')
  end

  # Buffered list-of-records both ways.
  batch = AsyncDemo.run_batch(%w[a b c])
  expect(batch.map(&:value) == ['completed: a', 'completed: b', 'completed: c'],
         "run_batch values (got #{batch.map(&:value)})")
  expect(batch.all?(&:success), 'run_batch success flags')
  expect(AsyncDemo.run_batch([]) == [], 'run_batch of nothing')

  # Direct scalar through the async callback, from many threads at once.
  expect(AsyncDemo.run_n_tasks(7) == 7, 'run_n_tasks echoes n')
  results = Array.new(8) { |i| Thread.new { Array.new(25) { |j| AsyncDemo.run_n_tasks(i * 100 + j) } } }.map(&:value)
  expect(results.each_with_index.all? { |rs, i| rs == Array.new(25) { |j| i * 100 + j } },
         'concurrent async calls each receive their own result')

  # --- Cancellation --------------------------------------------------------
  # Without cancellation, wait completes with the milliseconds waited.
  expect(AsyncDemo.wait(20) == 20, 'wait completes after its timeout')
  idle = AsyncDemo::CancelToken.new
  expect(AsyncDemo.wait(5, cancel: idle) == 5, 'an untouched token changes nothing')
  idle.close

  # Cancelling the token from another thread completes the call at once with
  # AsyncDemo::Cancelled (code -5) instead of waiting out the timeout.
  token = AsyncDemo::CancelToken.new
  canceller = Thread.new do
    sleep 0.05
    token.cancel
  end
  started = Time.now
  begin
    AsyncDemo.wait(60_000, cancel: token)
    raise 'expected AsyncDemo::Cancelled'
  rescue AsyncDemo::Cancelled => e
    expect(e.code == -5 && e.code == AsyncDemo::CANCELLED_ERROR_CODE, "Cancelled code -5 (got #{e.code})")
    expect(e.is_a?(AsyncDemo::Error), 'Cancelled is an AsyncDemo::Error')
  end
  expect(Time.now - started < 5, "cancellation didn't wait for the timeout")
  canceller.join
  expect(token.cancelled?, 'token reports cancelled')

  # A token cancelled before the call never runs the body.
  begin
    AsyncDemo.wait(60_000, cancel: token)
    raise 'expected AsyncDemo::Cancelled for a pre-cancelled token'
  rescue AsyncDemo::Cancelled => e
    expect(e.code == -5, "pre-cancelled code (got #{e.code})")
  end
  token.close

  # Interrupting the waiting thread cancels the call's private token, so the
  # producer drops its work (active_callbacks settles) instead of running out
  # the timeout.
  interrupted = Class.new(StandardError)
  waiter = Thread.new do
    AsyncDemo.wait(60_000)
    :completed
  rescue interrupted
    :interrupted
  end
  sleep 0.1
  started = Time.now
  waiter.raise(interrupted)
  expect(waiter.value == :interrupted, 'the interrupted wait raised the interruption')
  expect(settle_tasks, 'interrupting the wait cancelled the producer work')
  expect(Time.now - started < 5, "the interrupted work didn't run out its timeout")

  # Sync functions beside the async ones; every task body has completed by
  # the time its callback fires.
  expect(settle_tasks, 'active_callbacks settles to zero')
end

puts 'ruby/async-demo: OK'
