# frozen_string_literal: true
# Conformance consumer: kvstore sample, Ruby target.
#
# Full-surface drive of the ABI 2 wrapper: the reference-counted Store
# interface (the open(path) factory constructor, sync methods, the
# Enumerator-backed list_keys, the blocking compact bridge over the async
# ABI, the deprecated legacy_put, the default_capacity static, `close` and
# `dup` semantics), the typed KvError domain (codes 1001-1004) with per-code
# subclasses raised by throwing members, the Entry record as a plain value
# class decoded from a value buffer, a Ruby class including the generated
# `EvictionListener` mixin (attach, fire on delete and on expiry-on-read,
# detach by returning false, replace, clear, and a raising listener surfacing
# as the brand error with code -4), the object graph (share() wraps the SAME
# object, fork(), Store? both ways in larger(), a Store inside the StoreInfo
# record from describe(), [Store] from open_many(), and [Store] plus StoreInfo?
# as parameters to total_count()), and the cross-module
# `Kvstore.get_stats(store)`. The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "kvstore"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Size of the generated registry of live callback implementations: the only
# observable proof that the producer's `free` entry ran exactly once.
def live_callbacks
  Kvstore.instance_variable_get(:@wv_cb_registry).size
end

# An eviction listener recording (key, reason) pairs; returns false (detach)
# once `keep_after` evictions were seen, and raises for `raise_on`.
class Listener
  include Kvstore::EvictionListener

  attr_reader :seen, :entries

  def initialize(keep_after: nil, raise_on: nil)
    @keep_after = keep_after
    @raise_on = raise_on
    @seen = []
    @entries = []
  end

  def on_evict(entry, reason)
    @seen << [entry.key, reason]
    @entries << entry
    raise "listener exploded on #{entry.key}" if entry.key == @raise_on

    @keep_after.nil? || @seen.length < @keep_after
  end
end

expect(Kvstore::ABI_VERSION == 2, "bindings target ABI revision 2")
expect(Kvstore::EvictionReason::DELETED.zero?, "EvictionReason::DELETED == 0")
expect(Kvstore::EvictionReason::EXPIRED == 1, "EvictionReason::EXPIRED == 1")

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
expect(store.is_a?(Kvstore::Store) && !store.closed?, "open returns an open Store")
payload = "\x01\x02\x03".b

# put takes a buffered `i64?` TTL: nil and a present value both encode.
expect(store.put("alpha", payload, Kvstore::EntryKind::PERSISTENT, nil) == true, "put alpha")
expect(store.put("beta", payload, Kvstore::EntryKind::VOLATILE, 3600) == true, "put beta")
expect(store.count == 2, "count == 2")

# Iterator-backed list-of-string return, optionally prefix-filtered (the
# `string?` prefix is a buffered optional parameter). The Enumerator is lazy
# and releases the producer iterator on early termination.
expect(store.list_keys(nil).is_a?(Enumerator), "list_keys returns an Enumerator")
keys = store.list_keys(nil).to_a
expect(keys == %w[alpha beta], "list_keys values in sorted order (got #{keys})")
expect(store.list_keys("al").to_a == %w[alpha], "list_keys prefix filter")
expect(store.list_keys("zz").to_a == [], "list_keys unmatched prefix is empty")
expect(store.list_keys(nil).first(1) == %w[alpha], "early break through first(1)")
seen = []
store.list_keys(nil).each { |k| seen << k.upcase }
expect(seen == %w[ALPHA BETA], "block iteration")

# Optional record return: get decodes a buffered `Entry?` into a plain value
# object covering every complex field type.
alpha = store.get("alpha")
expect(!alpha.nil?, "get alpha present")
expect(alpha.is_a?(Kvstore::Entry), "get returns an Entry value")
expect(alpha.id.positive?, "entry id positive")
expect(alpha.key == "alpha", "entry key")
expect(alpha.value == payload, "entry value bytes")
expect(alpha.expires_at.nil?, "alpha expires_at nil")
expect(alpha.tags == [], "alpha tags empty")
expect(alpha.metadata == {}, "alpha metadata empty")

# Repeated gets decode fresh snapshots that compare structurally equal.
expect(store.get("alpha") == alpha, "second get equals first snapshot")

beta = store.get("beta")
expect(!beta.expires_at.nil? && beta.expires_at.positive?, "beta expires_at present")
expect(beta.expires_at - beta.created_at == 3600, "beta TTL applied (got #{beta.expires_at - beta.created_at})")

# A missing key raises the typed KeyNotFound (1001).
begin
  store.get("missing")
  raise "expected KvError::KeyNotFound"
rescue Kvstore::KvError::KeyNotFound => e
  expect(e.code == 1001, "KeyNotFound code == 1001 (got #{e.code})")
  expect(e.message == "key not found", "KeyNotFound default message (got #{e.message.inspect})")
end

# Rescuing the domain base class catches any code in the domain.
begin
  store.get("missing")
  raise "expected KvError"
rescue Kvstore::KvError => e
  expect(e.code == 1001, "domain rescue sees KeyNotFound (got #{e.code})")
end

# The deprecated method still works (and warns on stderr).
expect(store.legacy_put("legacy", payload) == true, "legacy_put")
expect(store.delete("legacy") == true, "delete legacy")
expect(store.delete("legacy") == false, "second delete returns false")

# Entry is a plain value class: keyword construction, non-empty native
# list/map fields, a nil-able optional, and structural equality.
built = Kvstore::Entry.new(
  id: 7,
  key: "built",
  value: payload,
  created_at: 1000,
  expires_at: nil,
  tags: %w[hot fast],
  metadata: { "source" => "test", "env" => "prod" }
)
expect(built.tags.sort == %w[fast hot], "built tags (got #{built.tags})")
expect(built.metadata == { "source" => "test", "env" => "prod" }, "built metadata")
expect(built.expires_at.nil?, "built expires_at nil")
same = Kvstore::Entry.new(
  id: 7, key: "built", value: payload, created_at: 1000,
  expires_at: nil, tags: %w[hot fast],
  metadata: { "source" => "test", "env" => "prod" }
)
expect(built == same, "entries compare structurally")
expect(built != alpha, "different entries are unequal")

# The cross-module call under test: kv.stats.get_stats borrows the kv.Store
# interface pointer and returns the Stats record as a value.
stats = Kvstore.get_stats(store)
expect(stats.is_a?(Kvstore::Stats), "get_stats returns a Stats value")
expect(stats.total_entries == 2, "total_entries == 2 (got #{stats.total_entries})")
expect(stats.total_bytes == 6, "total_bytes == 6 (got #{stats.total_bytes})")
expect(stats.expired_entries == 0, "expired_entries == 0 (got #{stats.expired_entries})")

# --- Eviction listener (callback interface) ------------------------------
baseline = live_callbacks

listener = Listener.new
store.set_eviction_listener(listener)
expect(live_callbacks == baseline + 1, "listener registered")

# delete fires on_evict synchronously with the removed Entry and DELETED.
expect(store.delete("beta") == true, "delete beta")
expect(listener.seen == [["beta", Kvstore::EvictionReason::DELETED]], "eviction fired for beta (got #{listener.seen})")
evicted = listener.entries[0]
expect(evicted.is_a?(Kvstore::Entry), "on_evict receives an Entry value")
expect(evicted.value == payload && evicted.expires_at == beta.expires_at, "evicted entry matches the stored one")
expect(evicted == beta, "evicted entry equals the earlier snapshot")

# An entry whose TTL already elapsed is evicted on read: get raises Expired
# (1002) and the listener sees EXPIRED.
expect(store.put("stale", "zz".b, Kvstore::EntryKind::VOLATILE, -1) == true, "put stale")
begin
  store.get("stale")
  raise "expected KvError::Expired"
rescue Kvstore::KvError::Expired => e
  expect(e.code == 1002, "Expired code == 1002 (got #{e.code})")
end
expect(listener.seen.last == ["stale", Kvstore::EvictionReason::EXPIRED], "expiry eviction (got #{listener.seen.last})")
expect(listener.entries.last.value == "zz".b, "expired entry payload")
expect(store.count == 1, "stale entry gone after eviction")

# Replacing the listener frees the previous one; clearing frees the current.
second = Listener.new
store.set_eviction_listener(second)
expect(live_callbacks == baseline + 1, "replaced listener freed, replacement registered")
store.put("gamma", payload, Kvstore::EntryKind::VOLATILE, nil)
store.delete("gamma")
expect(listener.seen.length == 2, "replaced listener no longer notified")
expect(second.seen == [["gamma", Kvstore::EvictionReason::DELETED]], "replacement notified (got #{second.seen})")
store.clear_eviction_listener
expect(live_callbacks == baseline, "clear_eviction_listener freed the listener")
store.put("delta", payload, Kvstore::EntryKind::VOLATILE, nil)
store.delete("delta")
expect(second.seen.length == 1, "no notification after clear")
store.clear_eviction_listener

# Returning false detaches (and frees) the listener after that eviction.
brief = Listener.new(keep_after: 2)
store.set_eviction_listener(brief)
store.put("one", payload, Kvstore::EntryKind::VOLATILE, nil)
store.put("two", payload, Kvstore::EntryKind::VOLATILE, nil)
store.put("three", payload, Kvstore::EntryKind::VOLATILE, nil)
store.delete("one")
expect(live_callbacks == baseline + 1, "listener retained after returning true")
store.delete("two")
expect(live_callbacks == baseline, "listener freed after returning false")
store.delete("three")
expect(brief.seen.map(&:first) == %w[one two], "detached listener misses the third eviction (got #{brief.seen})")

# A Ruby exception inside on_evict surfaces to the caller of delete as the
# brand error with FOREIGN_ERROR_CODE (-4), not as a KvError; the entry was
# already removed, the VM is fine, and the store keeps working.
angry = Listener.new(raise_on: "kaboom")
store.set_eviction_listener(angry)
store.put("kaboom", payload, Kvstore::EntryKind::VOLATILE, nil)
begin
  store.delete("kaboom")
  raise "expected Kvstore::Error from a raising listener"
rescue Kvstore::KvError
  raise "a foreign error must not be mapped to a KvError"
rescue Kvstore::Error => e
  expect(e.code == Kvstore::FOREIGN_ERROR_CODE, "raising listener -> code -4 (got #{e.code})")
  expect(e.code == -4, "FOREIGN_ERROR_CODE is -4")
  expect(e.message.include?("listener exploded on kaboom"), "foreign error text (got #{e.message.inspect})")
end
expect(store.count == 1, "entry removed despite the listener failure (got #{store.count})")
expect(store.delete("kaboom") == false, "kaboom is gone")
expect(store.put("calm", payload, Kvstore::EntryKind::VOLATILE, nil) && store.delete("calm"),
       "store usable after a foreign error")
expect(angry.seen.map(&:first) == %w[kaboom calm], "raising listener stays attached (got #{angry.seen})")
store.clear_eviction_listener
expect(live_callbacks == baseline, "all listeners released")

# --- Object graph --------------------------------------------------------
# share() returns a wrapper to the SAME object: writes through one are visible
# through the other, and closing one wrapper leaves the object alive.
shared = store.share
expect(shared.is_a?(Kvstore::Store), "share returns a Store")
expect(!shared.equal?(store), "share returns a distinct wrapper")
expect(shared.handle.address == store.handle.address, "share wraps the same object")
expect(shared.put("via-shared", payload, Kvstore::EntryKind::VOLATILE, nil), "put through the shared wrapper")
expect(store.count == 2, "write through share visible in the original (got #{store.count})")
expect(store.get("via-shared").value == payload, "entry readable through the original")
shared.close
shared.close
expect(shared.closed?, "shared wrapper closed idempotently")
expect(store.count == 2, "object survives closing the shared wrapper")

# dup and clone also mint independent wrappers with their own reference.
twin = store.dup
expect(twin.handle.address == store.handle.address, "dup wraps the same object")
expect(twin.count == 2, "dup sees the same entries")
twin.close
expect(store.count == 2, "object survives closing the dup")

# fork() is a new, independent object.
forked = store.fork
expect(forked.handle.address != store.handle.address, "fork is a different object")
expect(forked.count == 2, "fork copied the live entries (got #{forked.count})")
forked.put("only-in-fork", payload, Kvstore::EntryKind::VOLATILE, nil)
expect(forked.count == 3 && store.count == 2, "fork is independent of the original")

# larger(): Store? in and out.
empty = Kvstore::Store.open("/tmp/conformance-kvstore-rb-empty")
expect(empty.larger(nil).nil?, "larger(nil) on an empty store is nil")
own = store.larger(nil)
expect(!own.nil? && own.handle.address == store.handle.address, "larger(nil) on a non-empty store is itself")
own.close
bigger = store.larger(forked)
expect(bigger.handle.address == forked.handle.address, "larger picks the fuller store")
bigger.close
still = forked.larger(empty)
expect(still.handle.address == forked.handle.address, "larger keeps self when other is smaller")
still.close
expect(forked.count == 3 && empty.count.zero?, "larger left both stores intact")

# describe(): a record carrying the object itself plus an optional object.
info = store.describe("primary", nil)
expect(info.is_a?(Kvstore::StoreInfo), "describe returns a StoreInfo")
expect(info.label == "primary", "info.label (got #{info.label.inspect})")
expect(info.count == 2, "info.count (got #{info.count})")
expect(info.mirror.nil?, "info.mirror nil when absent")
expect(info.store.is_a?(Kvstore::Store), "info.store is a Store wrapper")
expect(info.store.handle.address == store.handle.address, "info.store is the described object")
expect(info.store.count == 2, "info.store is usable")
mirrored = store.describe("mirrored", forked)
expect(!mirrored.mirror.nil? && mirrored.mirror.handle.address == forked.handle.address, "info.mirror is the passed store")
expect(mirrored.mirror.count == 3, "info.mirror is usable")
expect(forked.count == 3, "borrowed mirror parameter still alive")

# open_many(): a list of objects as a return; a bad path raises the typed error
# for the whole call.
many = Kvstore::Store.open_many(["/tmp/a", "/tmp/b", "/tmp/c"])
expect(many.is_a?(Array) && many.length == 3, "open_many returns 3 stores")
expect(many.all? { |s| s.is_a?(Kvstore::Store) && !s.closed? }, "every element is an open Store")
expect(many.map { |s| s.handle.address }.uniq.length == 3, "open_many stores are distinct objects")
many[0].put("m0", payload, Kvstore::EntryKind::VOLATILE, nil)
many[2].put("m2a", payload, Kvstore::EntryKind::VOLATILE, nil)
many[2].put("m2b", payload, Kvstore::EntryKind::VOLATILE, nil)
expect(many.map(&:count) == [1, 0, 2], "open_many stores hold independent state")
expect(Kvstore::Store.open_many([]) == [], "open_many of nothing is empty")
begin
  Kvstore::Store.open_many(["/tmp/ok", ""])
  raise "expected KvError::IoError from open_many"
rescue Kvstore::KvError::IoError => e
  expect(e.code == 1004, "open_many IoError code (got #{e.code})")
end

# total_count(): a list of objects and an optional record holding an object as
# parameters (the encoder mints one reference per object token).
expect(Kvstore::Store.total_count([], nil).zero?, "total_count of nothing")
expect(Kvstore::Store.total_count(many, nil) == 3, "total_count over open_many (got #{Kvstore::Store.total_count(many, nil)})")
expect(Kvstore::Store.total_count(many, info) == 5, "total_count with the described store (got #{Kvstore::Store.total_count(many, info)})")
expect(Kvstore::Store.total_count([store, store, forked], nil) == 7, "the same object may appear repeatedly")
ruby_info = Kvstore::StoreInfo.new(label: "ruby-built", store: forked, mirror: store, count: 0)
expect(Kvstore::Store.total_count([empty], ruby_info) == 3, "Ruby-built StoreInfo encodes its object fields")
expect(Kvstore::Store.total_count([], mirrored) == 2, "StoreInfo with a mirror encodes both objects")
expect(store.count == 2 && forked.count == 3 && many.map(&:count) == [1, 0, 2],
       "borrowed parameter objects survive total_count")

# --- Async ---------------------------------------------------------------
# An immediately-expired entry gives compact 3 bytes to reclaim; the wrapper
# blocks on a Queue fed from the producer's worker-thread callback.
expect(store.put("doomed", payload, Kvstore::EntryKind::VOLATILE, 0) == true, "put doomed")
reclaimed = store.compact
expect(reclaimed == 3, "compact reclaimed 3 bytes (got #{reclaimed})")
expect(store.compact.zero?, "second compact reclaims nothing")
expect(store.count == 2, "live entries untouched by compact")

# clear drops everything left in one call.
store.clear
expect(store.count.zero?, "count == 0 after clear")
expect(store.list_keys(nil).to_a == [], "no keys after clear")

# --- Release -------------------------------------------------------------
# Every wrapper closes exactly once; a second close is a no-op; the object
# itself lives until its last wrapper (info.store, mirrored.store, ...) goes.
store.close
store.close
expect(store.closed?, "store closed")
begin
  store.count
  raise "expected Error when using a closed Store"
rescue Kvstore::Error => e
  expect(e.message.include?("after close"), "use-after-close message (got #{e.message.inspect})")
end
expect(info.store.count.zero?, "object still alive through the record's reference")
info.store.close
mirrored.store.close
mirrored.mirror.close
expect(forked.count == 3, "fork still alive after the mirror wrapper closed")
forked.close
empty.close
many.each(&:close)
many.each(&:close)
expect(many.all?(&:closed?), "every open_many store closed")
GC.start
GC.start

puts "ruby/kvstore: OK"
