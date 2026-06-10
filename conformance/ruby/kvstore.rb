# frozen_string_literal: true
# Conformance consumer: kvstore sample, Ruby target.
#
# Full-surface drive of the generated FFI wrapper: typed-handle returns
# (Store), optional struct returns (Entry or nil) with bytes / optional-scalar
# / list / map getters, the fluent EntryBuilder (list + map *input*
# marshalling), the iterator-backed list_keys, the *cross-module*
# `Kvstore.get_stats(store)` (Stats lives in kv.stats, store is a kv.Store —
# the wrapper must dispatch to `weaveffi_kv_stats_get_stats`), the
# FFI::Function eviction listener (register -> fire on delete -> unregister),
# and the blocking thread+queue compact_async bridge. The cdylib is selected
# via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "kvstore"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

store = Kvstore.open_store("/tmp/conformance-kvstore-rb")
payload = "\x01\x02\x03".b

expect(Kvstore.put(store, "alpha", payload, Kvstore::EntryKind::PERSISTENT, nil) == true, "put alpha")
expect(Kvstore.put(store, "beta", payload, Kvstore::EntryKind::VOLATILE, 3600) == true, "put beta")
expect(Kvstore.count(store) == 2, "count == 2")

# Iterator-backed list-of-string return.
keys = Kvstore.list_keys(store, nil).to_a.sort
expect(keys == %w[alpha beta], "list_keys values (got #{keys})")

# Optional struct return + getters over every complex field type.
alpha = Kvstore.get(store, "alpha")
expect(!alpha.nil?, "get alpha present")
expect(alpha.id.positive?, "entry id positive")
expect(alpha.key == "alpha", "entry key")
expect(alpha.value == payload, "entry value bytes")
expect(alpha.expires_at.nil?, "alpha expires_at nil")
expect(alpha.tags == [], "alpha tags empty")
expect(alpha.metadata == {}, "alpha metadata empty")

beta = Kvstore.get(store, "beta")
expect(!beta.nil? && !beta.expires_at.nil? && beta.expires_at.positive?, "beta expires_at present")

# Builder round-trips non-empty list/map inputs through the C `create`.
built = Kvstore::EntryBuilder.new
  .with_id(7)
  .with_key("built")
  .with_value(payload)
  .with_created_at(1000)
  .with_expires_at(nil)
  .with_tags(%w[hot fast])
  .with_metadata("source" => "test", "env" => "prod")
  .build
expect(built.tags.sort == %w[fast hot], "built tags (got #{built.tags})")
expect(built.metadata == { "source" => "test", "env" => "prod" }, "built metadata")
expect(built.expires_at.nil?, "built expires_at nil")

# The cross-module call under test.
stats = Kvstore.get_stats(store)
expect(stats.total_entries == 2, "total_entries == 2 (got #{stats.total_entries})")
expect(stats.expired_entries == 0, "expired_entries == 0 (got #{stats.expired_entries})")

# Eviction listener: delete fires the FFI::Function trampoline synchronously.
evicted = []
sub = Kvstore.register_eviction_listener { |key| evicted << key }
expect(sub.positive?, "listener id positive")
expect(Kvstore.delete(store, "beta") == true, "delete beta")
expect(evicted == %w[beta], "eviction fired for beta (got #{evicted})")

# Unregister stops delivery.
Kvstore.unregister_eviction_listener(sub)
expect(Kvstore.delete(store, "alpha") == true, "delete alpha")
expect(evicted == %w[beta], "no eviction after unregister (got #{evicted})")

# Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
# wrapper blocks on a Queue fed from the producer's worker-thread callback.
expect(Kvstore.put(store, "doomed", payload, Kvstore::EntryKind::VOLATILE, 0) == true, "put doomed")
reclaimed = Kvstore.compact_async(store)
expect(reclaimed == 3, "compact reclaimed 3 bytes (got #{reclaimed})")
expect(Kvstore.count(store).zero?, "store empty after deletes + compact")

# Store/Stats wrap their handles in FFI::AutoPointer and free on GC; calling
# the explicit close_store as well would double-free, so we rely on the GC.

puts "ruby/kvstore: OK"
