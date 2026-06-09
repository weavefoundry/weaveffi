# frozen_string_literal: true
# Conformance consumer: kvstore sample, Ruby target.
#
# The key assertion is the *cross-module* call `Kvstore.get_stats(store)`:
# `Stats` lives in the `kv.stats` submodule while the `store` handle is a
# `kv.Store` from the parent module. The generated FFI wrapper must dispatch to
# the owner's C symbol `weaveffi_kv_stats_get_stats` and pass `store.handle`.
# The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "kvstore"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

store = Kvstore.open_store("/tmp/kvstore-conformance-rb")

expect(Kvstore.put(store, "a", "hi", Kvstore::EntryKind::PERSISTENT, nil) == true, "put a")
expect(Kvstore.put(store, "b", "bye", Kvstore::EntryKind::PERSISTENT, nil) == true, "put b")
expect(Kvstore.count(store) == 2, "count == 2")

# The cross-module call under test.
stats = Kvstore.get_stats(store)
expect(stats.total_entries == 2, "total_entries == 2 (got #{stats.total_entries})")
expect(stats.total_bytes == 5, "total_bytes == 5 (got #{stats.total_bytes})")
expect(stats.expired_entries == 0, "expired_entries == 0 (got #{stats.expired_entries})")

# `Store`/`Stats` wrap their handles in FFI::AutoPointer and free on GC; calling
# the explicit `close_store` as well would double-free, so we rely on the GC.

puts "ruby/kvstore: OK"
