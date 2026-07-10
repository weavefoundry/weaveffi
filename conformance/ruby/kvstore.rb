# frozen_string_literal: true
# Conformance consumer: kvstore sample, Ruby target.
#
# Full-surface drive of the 0.5.0 wrapper: the Store interface (the open(path)
# factory constructor, sync methods, the iterator-backed list_keys, the
# blocking compact bridge over the async ABI, the deprecated legacy_put, and
# the default_capacity static), the typed KvError domain (codes 1001-1004)
# with per-code subclasses raised by throwing members, the Entry record with
# bytes / optional-scalar / list / map getters plus the fluent EntryBuilder,
# the FFI::Function eviction listener (register -> fire on delete ->
# unregister), and the cross-module `Kvstore.get_stats(store)` (Stats lives in
# kv.stats; the store passes as a borrowed interface pointer). The cdylib is
# selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "kvstore"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Interface static: a class method with no self slot.
expect(Kvstore::Store.default_capacity == 1_000_000, "default_capacity")

# The open(path) factory throws the typed IoError (1004) on an empty path.
begin
  Kvstore::Store.open("")
  raise "expected KvError::IoError for empty path"
rescue Kvstore::KvError::IoError => e
  expect(e.code == 1004, "IoError code == 1004 (got #{e.code})")
  expect(e.is_a?(Kvstore::KvError), "IoError is a KvError")
  expect(e.is_a?(Kvstore::Error), "domain errors subclass Kvstore::Error")
end

store = Kvstore::Store.open("/tmp/conformance-kvstore-rb")
payload = "\x01\x02\x03".b

expect(store.put("alpha", payload, Kvstore::EntryKind::PERSISTENT, nil) == true, "put alpha")
expect(store.put("beta", payload, Kvstore::EntryKind::VOLATILE, 3600) == true, "put beta")
expect(store.count == 2, "count == 2")

# Iterator-backed list-of-string return, optionally prefix-filtered.
keys = store.list_keys(nil).to_a.sort
expect(keys == %w[alpha beta], "list_keys values (got #{keys})")
expect(store.list_keys("al").to_a == %w[alpha], "list_keys prefix filter")

# Optional struct return + getters over every complex field type.
alpha = store.get("alpha")
expect(!alpha.nil?, "get alpha present")
expect(alpha.id.positive?, "entry id positive")
expect(alpha.key == "alpha", "entry key")
expect(alpha.value == payload, "entry value bytes")
expect(alpha.expires_at.nil?, "alpha expires_at nil")
expect(alpha.tags == [], "alpha tags empty")
expect(alpha.metadata == {}, "alpha metadata empty")

beta = store.get("beta")
expect(!beta.expires_at.nil? && beta.expires_at.positive?, "beta expires_at present")

# A missing key raises the typed KeyNotFound (1001).
begin
  store.get("missing")
  raise "expected KvError::KeyNotFound"
rescue Kvstore::KvError::KeyNotFound => e
  expect(e.code == 1001, "KeyNotFound code == 1001 (got #{e.code})")
end

# The deprecated method still works (and warns on stderr).
expect(store.legacy_put("legacy", payload) == true, "legacy_put")
expect(store.delete("legacy") == true, "delete legacy")

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

# The cross-module call under test: kv.stats.get_stats borrows the kv.Store
# interface pointer.
stats = Kvstore.get_stats(store)
expect(stats.total_entries == 2, "total_entries == 2 (got #{stats.total_entries})")
expect(stats.total_bytes == 6, "total_bytes == 6 (got #{stats.total_bytes})")
expect(stats.expired_entries == 0, "expired_entries == 0 (got #{stats.expired_entries})")

# Eviction listener: delete fires the FFI::Function trampoline synchronously.
evicted = []
sub = Kvstore.register_eviction_listener { |key| evicted << key }
expect(sub.positive?, "listener id positive")
expect(store.delete("beta") == true, "delete beta")
expect(evicted == %w[beta], "eviction fired for beta (got #{evicted})")

# Unregister stops delivery.
Kvstore.unregister_eviction_listener(sub)
expect(store.delete("alpha") == true, "delete alpha")
expect(evicted == %w[beta], "no eviction after unregister (got #{evicted})")

# Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
# wrapper blocks on a Queue fed from the producer's worker-thread callback.
expect(store.put("doomed", payload, Kvstore::EntryKind::VOLATILE, 0) == true, "put doomed")
reclaimed = store.compact
expect(reclaimed == 3, "compact reclaimed 3 bytes (got #{reclaimed})")
expect(store.count.zero?, "store empty after deletes + compact")

# clear drops everything left in one call.
expect(store.put("temp", payload, Kvstore::EntryKind::VOLATILE, nil) == true, "put temp")
expect(store.count == 1, "count == 1 before clear")
store.clear
expect(store.count.zero?, "count == 0 after clear")

# Explicit destroy releases the object early; the AutoPointer's GC release is
# then a no-op, so no double-free.
store.destroy

puts "ruby/kvstore: OK"
