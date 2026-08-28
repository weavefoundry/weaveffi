# frozen_string_literal: true
# Conformance consumer: async-demo sample, Ruby target.
#
# Drives the blocking async bridge end to end: `run_task` blocks the calling
# thread until the producer's completion callback fires and decodes the
# TaskResult value class from a value buffer, an empty name raises the typed
# TaskError::InvalidName subclass, `run_batch` round-trips a list of records
# through value buffers both ways, `run_n_tasks` returns a direct scalar
# through the callback, `cancel_task` is a plain sync call, and
# `active_callbacks` is back to zero once every task body has completed. The
# cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "async_demo"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Async record return: blocks until the worker-thread callback delivers the
# encoded TaskResult.
result = WeaveFFI.run_task("alpha")
expect(result.id.positive?, "run_task assigns an id")
expect(result.value == "completed: alpha", "run_task value (got #{result.value})")
expect(result.success == true, "run_task success flag")

# Typed async error: the empty name settles with the InvalidName subclass.
begin
  WeaveFFI.run_task("")
  expect(false, "expected TaskError::InvalidName for empty name")
rescue WeaveFFI::TaskError::InvalidName => e
  expect(e.code == 1, "InvalidName carries code 1 (got #{e.code})")
  expect(e.is_a?(WeaveFFI::TaskError), "subclass of TaskError")
  expect(e.is_a?(WeaveFFI::Error), "subclass of the brand error")
end

# Buffered list-of-records both ways.
batch = WeaveFFI.run_batch(%w[a b c])
expect(batch.map(&:value) == ["completed: a", "completed: b", "completed: c"],
       "run_batch values (got #{batch.map(&:value)})")
expect(batch.all?(&:success), "run_batch success flags")

# Direct scalar through the async callback.
expect(WeaveFFI.run_n_tasks(7) == 7, "run_n_tasks echoes n")

# Sync functions beside the async ones.
expect(WeaveFFI.cancel_task(1) == false, "cancel_task reports not cancelled")

# Every spawned task body has completed by the time its callback fires.
expect(WeaveFFI.active_callbacks.zero?, "active_callbacks settles to zero")

puts "ruby async-demo conformance: OK"
