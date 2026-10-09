# frozen_string_literal: true

# Conformance consumer: kvstore sample, the feature-complete producer, Ruby
# target.
#
# The `Store` class (a throwing factory, the `new` constructor, methods,
# statics, the deprecated `size`, close), typed `KvError`s with their
# payload fields, records with bytes, optional, list, and map fields, lazy
# iterators of strings, records, and objects, three callback interfaces
# implemented in Ruby (a `Listener` that is retained, filtered, detached
# when it raises, and called from a producer thread during compaction; a
# `Policy` with a record return, a typed error raised back through `put`,
# object parameters and returns, and malformed returns; a `Loader` passed as
# an optional callback with string, bytes, and optional-object returns),
# objects in every buffered position, blocking async calls with `cancel:`
# tokens, the nested `kv.stats` module, the sibling `report` root, and
# leak-free teardown. `_wv_debug_live(1)` (live callbacks) shows when the
# producer released an implementation.

require_relative 'support'
require 'kvstore'

Store = Kvstore::Store
EntryKind = Kvstore::EntryKind
KvError = Kvstore::KvError

def callbacks
  Kvstore._wv_debug_live(1)
end

def put(store, key, value, kind = EntryKind::PERSISTENT, ttl = nil)
  store.put(key, value.b, kind, ttl)
end

def entry(key, value, kind: EntryKind::PERSISTENT, version: 1)
  Kvstore::Entry.new(key: key, value: value.b, kind: kind, version: version, expires_at: nil, tags: [],
                     metadata: {})
end

def key_not_found(key, msg)
  e = expect_raise(KvError::KeyNotFound, msg) { yield }
  expect(e.is_a?(KvError) && e.code == 1001 && e.key == key, "#{msg}: KeyNotFound for #{key} (got #{e.key})")
  e
end

# Runs the block with Ruby warnings off (the deprecated `size` warns).
def quietly
  verbose = $VERBOSE
  $VERBOSE = nil
  yield
ensure
  $VERBOSE = verbose
end

# A listener recording every change it's told about, and on which thread.
class Recorder
  include Kvstore::Listener

  def initialize(skip: nil, fail_on: nil)
    @skip = skip
    @fail_on = fail_on
    @lock = Mutex.new
    @changes = []
    @threads = []
  end

  def accepts(key)
    raise 'listener refused' if key == @fail_on

    key != @skip
  end

  def on_change(change)
    @lock.synchronize do
      @changes << change
      @threads << Thread.current
    end
  end

  def changes
    @lock.synchronize { @changes.dup }
  end

  def threads
    @lock.synchronize { @threads.dup }
  end

  def count(cls)
    changes.count { |c| c.is_a?(cls) && (!block_given? || yield(c)) }
  end
end

# A policy that rewrites admitted entries, vetoes secrets with a typed
# error, and routes `b/` keys to another store.
class Rewriter
  include Kvstore::Policy

  attr_reader :admitted

  def initialize(other)
    @other = other
    @admitted = 0
  end

  def admit(entry)
    @admitted += 1
    raise "admit saw version #{entry.version}" unless entry.version.zero?

    case entry.key
    when /\Asecret/
      raise KvError::Rejected.new('secrets are not stored', key: entry.key, reason: 'no secrets')
    when /\Aboom/
      raise 'policy exploded'
    when /\Agarbage/
      # An undeclared EntryKind: the producer rejects the record with -3.
      Kvstore::Entry.new(key: entry.key, value: entry.value, kind: 9, version: 0, expires_at: nil, tags: [],
                         metadata: {})
    else
      Kvstore::Entry.new(key: 'renamed', value: entry.value, kind: EntryKind::ENCRYPTED, version: entry.version,
                         expires_at: entry.expires_at, tags: ['admitted'], metadata: entry.metadata)
    end
  end

  def route(key, home)
    case key
    when %r{\Ab/} then @other
    when %r{\Anull/} then nil # a required Store: the producer rejects it with -3
    else home
    end
  end
end

# A loader with an optional fallback store.
class Source
  include Kvstore::Loader

  def initialize(fallback = nil)
    @store = fallback
  end

  def name
    'ruby-loader'
  end

  def fallback(key)
    key == 'fb' ? @store : nil
  end

  def load(key)
    case key
    when 'missing' then raise KvError::KeyNotFound.new('not in the loader', key: 'missing')
    when 'elsewhere' then raise KvError::KeyNotFound.new('not in the loader', key: 'other')
    when 'broken' then raise 'loader is broken'
    else "loaded:#{key}".b
    end
  end
end

def constructors
  e = expect_raise(KvError::InvalidPath, 'open("")') { Store.open('') }
  expect(e.code == 1004 && KvError::InvalidPath::CODE == 1004 && e.message == 'invalid path', 'InvalidPath')
  expect(e.instance_variables == [:@code], 'InvalidPath carries no payload fields')
  s = Store.new
  expect(s.path == 'memory', 'Store.new is in memory')
  expect(s.capacity == Store.default_capacity && Store.default_capacity == 1_000_000, 'default capacity')
  s.close
  opened = Kvstore.open_store('/async')
  expect(opened.is_a?(Store) && opened.path == '/async', 'open_store returns a Store')
  opened.close
  expect_raise(KvError::InvalidPath, 'open_store("")') { Kvstore.open_store('') }
end

def basics
  s = Store.open('/basics')
  expect(put(s, 'alpha', 'one') == entry('alpha', 'one'), 'put returns the stored entry')
  second = put(s, 'alpha', 'two', EntryKind::VOLATILE)
  expect(second.version == 2 && second.kind == EntryKind::VOLATILE, 'a second put bumps the version')
  expect(s.get('alpha').value == 'two'.b, 'get')
  e = key_not_found('nope', 'get a missing key') { s.get('nope') }
  expect(e.message == 'key not found: nope', "the producer's message (got #{e.message})")
  expect(s.find('alpha')&.version == 2 && s.find('nope').nil?, 'find')

  expect(put(s, 'ttl', 'x', EntryKind::VOLATILE, 10).expires_at == 10, 'a ttl sets expires_at')
  expect(s.now.zero?, 'the clock starts at zero')
  expect(s.tick(9) == 9 && s.count == 2, 'tick(9)')
  expect(s.tick(1) == 10 && s.count == 1, 'tick(1) expires the entry')
  e = expect_raise(KvError::Expired, 'get an expired key') { s.get('ttl') }
  expect(e.key == 'ttl' && e.expired_at == 10 && e.message == 'entry ttl expired at 10', 'Expired payload')
  key_not_found('ttl', 'the expired read removed it') { s.get('ttl') }

  s.set_capacity(1)
  expect(s.capacity == 1, 'set_capacity')
  put(s, 'alpha', 'three')
  e = expect_raise(KvError::StoreFull, 'a new key past the capacity') { put(s, 'beta', 'b') }
  expect(e.capacity == 1 && e.code == 1003, 'StoreFull payload')
  s.set_capacity(100)
  e = expect_raise(Kvstore::Error, 'an undeclared enum value') { put(s, 'k', 'v', 9) }
  expect(!e.is_a?(KvError) && e.code == -3, "an undeclared enum value is a marshalling failure (got #{e.code})")

  put(s, 'beta', 'b')
  expect(s.delete('beta') == true && s.delete('beta') == false, 'delete')
  expect(quietly { s.size } == s.count && s.count == 1, 'the deprecated size')
  expect(s.clear == 1 && s.count.zero?, 'clear')
  s.close
  closed = expect_raise(Kvstore::Error, 'a closed store') { s.count }
  expect(closed.message.include?('used after close'), closed.message)
  expect_raise(TypeError, 'a store of the wrong type') { Kvstore.summarize('not a store') }
end

def iterators
  s = Store.open('/iter')
  put(s, 'user.bob', 'b')
  put(s, 'user.alice', 'a')
  put(s, 'sys.x', 'xx')

  expect(s.keys.to_a == ['sys.x', 'user.alice', 'user.bob'], 'keys in order')
  key_not_found('zzz', 'a prefix that matches nothing') { s.keys('zzz').to_a }
  expect(s.keys('user.').first == 'user.alice', 'a lazy first key')
  expect(Kvstore._wv_debug_live(2).zero?, 'abandoning an iterator releases it')

  entries = s.entries('sys.').to_a
  expect(entries.length == 1 && entries[0].key == 'sys.x' && entries[0].value == 'xx'.b, 'entries')

  prefixes = ['user.', 'sys.', 'none.']
  parts = s.partition(prefixes).to_a
  expect(parts.length == 3, 'one store per prefix')
  parts.each_with_index do |p, i|
    expect(p.is_a?(Store) && p.path == prefixes[i], "partition #{i} path")
    expect(p.count == [2, 1, 0][i], "partition #{i} count")
    p.close
  end
  s.close
end

def listeners(main)
  base = callbacks
  s = Store.open('/listen')
  l = Recorder.new(skip: 'quiet')
  id = s.subscribe(l)
  expect(id.positive? && s.listener_count == 1 && callbacks == base + 1, 'subscribe')

  put(s, 'a', '1')
  last = l.changes.last
  expect(last.is_a?(Kvstore::Change::Put) && last.entry.version == 1 && last.replaced == false, 'a new key')
  put(s, 'a', '2')
  last = l.changes.last
  expect(last.is_a?(Kvstore::Change::Put) && last.entry.version == 2 && last.replaced == true, 'a replaced key')
  put(s, 'quiet', 'x')
  expect(l.count(Kvstore::Change::Put) == 2, 'accepts filters a key')
  s.delete('a')
  expect(l.changes.last == Kvstore::Change::Removed.new(key: 'a', expired: false), 'a delete')
  put(s, 'short', 'x', EntryKind::VOLATILE, 1)
  s.tick(1)
  expect_raise(KvError::Expired, 'an expired read') { s.get('short') }
  expect(l.changes.last == Kvstore::Change::Removed.new(key: 'short', expired: true), 'an expired read notifies')
  expect(s.clear == 1, 'clear leaves quiet')
  expect(l.changes.last == Kvstore::Change::Cleared.new(count: 1), 'a clear')
  expect(l.threads.all? { |t| t == main }, 'synchronous calls notify on the calling thread')

  expect(s.unsubscribe(id) == true && callbacks == base, 'unsubscribe releases the listener')
  expect(s.unsubscribe(id) == false && s.listener_count.zero?, 'a second unsubscribe')

  # A listener that raises is detached (and released); the put succeeds.
  failing = Recorder.new(fail_on: 'boom')
  s.subscribe(failing)
  put(s, 'fine', '1')
  expect(failing.count(Kvstore::Change::Put) == 1, 'the failing listener sees fine')
  put(s, 'boom', '1')
  expect(s.count == 2 && s.listener_count.zero? && callbacks == base, 'a raising listener is detached')

  # Releasing the store releases the listeners it still holds.
  s.subscribe(Recorder.new)
  s.subscribe(Recorder.new)
  expect(s.listener_count == 2 && callbacks == base + 2, 'two more listeners')
  s.close
  expect(callbacks == base, 'closing the store releases its listeners')
  expect_raise(TypeError, 'a nil listener') { Store.new.subscribe(nil) }
end

def policies
  base = callbacks
  s = Store.open('/policy')
  other = Store.open('/other')
  p = Rewriter.new(other)
  s.set_policy(p)
  expect(s.has_policy && callbacks == base + 1, 'set_policy')

  e = put(s, 'a', '1', EntryKind::VOLATILE)
  expect(e.key == 'a' && e.version == 1 && e.kind == EntryKind::ENCRYPTED, "admit's rewrite is stored")
  expect(e.tags == ['admitted'], "admit's tags")
  put(s, 'b/x', '2', EntryKind::VOLATILE)
  expect(s.count == 1 && other.count == 1, 'route redirects a write')

  e = expect_raise(KvError::Rejected, 'a typed error from admit') { put(s, 'secret', '3') }
  expect(e.code == 1005 && e.key == 'secret' && e.reason == 'no secrets', 'Rejected payload')
  expect(e.message == 'secrets are not stored', "the policy's message (got #{e.message})")
  e = expect_raise(Kvstore::Error, 'any other exception') { put(s, 'boom', '4') }
  expect(!e.is_a?(KvError) && e.code == -4 && e.message == 'policy exploded', "-4 (got #{e.code}: #{e.message})")
  e = expect_raise(Kvstore::Error, 'a malformed admit return') { put(s, 'garbage', '5') }
  expect(!e.is_a?(KvError) && e.code == -3, "a malformed record is -3 (got #{e.code}: #{e.message})")
  e = expect_raise(Kvstore::Error, 'a nil route return') { put(s, 'null/x', '6') }
  expect(!e.is_a?(KvError) && e.code == -3, "a null Store is -3 (got #{e.code}: #{e.message})")
  expect(s.count == 1 && other.count == 1 && p.admitted == 6, 'failed puts change nothing')

  s.set_policy(Rewriter.new(other))
  expect(callbacks == base + 1, 'replacing the policy releases the old one')
  s.set_policy(nil)
  expect(!s.has_policy && callbacks == base, 'set_policy(nil) releases it')
  put(s, 'secret', 'now allowed')
  expect(s.count == 2, 'no policy, no veto')
  other.close
  s.close
end

def loaders
  base = callbacks
  s = Store.open('/load')
  expect(s.get_or_load('k').nil?, 'no loader, no entry')
  loaded = s.get_or_load('k', Source.new)
  expect(loaded.value == 'loaded:k'.b && loaded.kind == EntryKind::VOLATILE, 'a loaded entry')
  expect(loaded.metadata == { 'source' => 'ruby-loader' }, 'the loader name is recorded')
  expect(callbacks == base, 'the loader is released after the call')
  expect(s.get_or_load('k', Source.new)&.version == 1, 'a hit skips the loader')

  backup = Store.open('/backup')
  put(backup, 'fb', 'from backup')
  copied = s.get_or_load('fb', Source.new(backup))
  expect(copied.value == 'from backup'.b && copied.kind == EntryKind::PERSISTENT, 'the fallback store')
  backup.close

  expect(s.get_or_load('missing', Source.new).nil?, 'KeyNotFound for this key is none')
  e = key_not_found('other', 'KeyNotFound for another key passes through') { s.get_or_load('elsewhere', Source.new) }
  expect(e.message == 'not in the loader', "the loader's message (got #{e.message})")
  e = expect_raise(Kvstore::Error, 'any other loader failure') { s.get_or_load('broken', Source.new) }
  expect(!e.is_a?(KvError) && e.code == -4 && e.message == 'loader is broken', "-4 (got #{e.code}: #{e.message})")
  expect(callbacks == base, 'every loader is released')
  s.close
end

def async_calls(main)
  s = Store.open('/async-calls')
  l = Recorder.new
  s.subscribe(l)
  put(s, 'old1', 'x', EntryKind::VOLATILE, 1)
  put(s, 'old2', 'x', EntryKind::VOLATILE, 1)
  put(s, 'keep', 'x')
  s.tick(5)

  token = Kvstore::CancelToken.new
  expect(s.compact(0, cancel: token) == 2, 'compact removes the expired entries')
  token.close
  expired = l.changes.each_index.select { |i| l.changes[i].is_a?(Kvstore::Change::Removed) && l.changes[i].expired }
  expect(expired.length == 2 && s.count == 1, 'the listener saw both removals')
  expect(expired.all? { |i| l.threads[i] != main }, 'compaction notifies on a producer thread')
  expect(s.compact(5).zero?, 'compact without a token')
  cancelled = Kvstore::CancelToken.new
  cancelled.cancel
  expect(cancelled.cancelled?, 'a cancelled token stays cancelled')
  expect_raise(Kvstore::Cancelled, 'an already-cancelled token') { s.compact(0, cancel: cancelled) }
  cancelled.close

  token = Kvstore::CancelToken.new
  canceller = Thread.new do
    sleep 0.02
    jobs = Store.active_jobs
    token.cancel
    jobs
  end
  started = Time.now
  e = expect_raise(Kvstore::Cancelled, 'cancelled mid-pause') { s.compact(60_000, cancel: token) }
  expect(e.code == -5 && e.is_a?(Kvstore::Error), 'Cancelled carries -5')
  expect(Time.now - started < 5, 'cancellation is prompt')
  expect(canceller.value >= 1, 'the pause was running')
  expect(eventually { Store.active_jobs.zero? }, 'the cancelled pause stopped cooperatively')
  token.close

  results = Array.new(32) { Thread.new { s.get_many(%w[keep gone keep]) } }.map(&:value)
  expect(results.all? { |r| r.length == 3 && r[0]&.key == 'keep' && r[1].nil? && r[2]&.key == 'keep' },
         '32 concurrent get_many calls')

  other = Store.open('/other')
  put(other, 'a', '123')
  stats = Kvstore.summarize_all([s, other])
  expect(stats == Kvstore::Stats.new(entries: 2, bytes: 4, by_kind: { EntryKind::PERSISTENT => 2 }), 'summarize_all')
  other.close
  s.close
end

def object_graph
  s = Store.open('/graph')
  put(s, 'k', 'v')

  shared = s.share
  put(s, 'via-original', 'v')
  expect(!shared.find('via-original').nil?, 'share is the same object')
  s.delete('via-original')
  s.close
  expect(shared.count == 1, 'still alive through the shared reference')

  fork = shared.fork
  expect(fork.count == 1 && fork.path == '/graph', 'fork copies the entries')
  put(fork, 'k2', 'v')
  expect(fork.count == 2 && shared.count == 1, 'fork is distinct')

  empty = Store.open('/empty')
  expect(empty.larger.nil?, 'larger(nil) on an empty store')
  bigger = empty.larger(fork)
  expect(bigger.count == 2, 'larger picks the fork')
  bigger.close
  itself = shared.larger
  put(itself, 'via-larger', 'v')
  expect(shared.count == 2, 'larger(nil) on a non-empty store is itself')
  itself.delete('via-larger')
  itself.close

  info = shared.describe('main', fork)
  expect(info.label == 'main' && info.count == 1, 'describe')
  expect(info.store.count == 1 && info.mirror.count == 2, 'describe carries both stores')
  put(info.store, 'via-info', 'v')
  expect(shared.count == 2, 'describe carries this store itself')
  shared.delete('via-info')

  opened = Store.open_many(['/a', '/b'])
  expect(opened.map(&:path) == ['/a', '/b'], 'open_many')
  expect_raise(KvError::InvalidPath, 'open_many with a bad path') { Store.open_many(['/a', '']) }

  first = Kvstore::StoreInfo.new(label: 'first', store: opened[0], mirror: nil, count: 0)
  named = Store.by_label([info, first])
  expect(named.keys.sort == %w[first main], 'by_label keys')
  expect(named['main'].count == 1 && named['first'].path == '/a', 'by_label values')

  put(opened[0], 'm', '1')
  expect(Store.total_count([opened[0], opened[1], fork], named, info) == 6, 'total_count with extra')
  expect(Store.total_count([opened[0], opened[1], fork], named) == 5, 'total_count without extra')
  expect(shared.count == 1 && fork.count == 2 && opened[0].count == 1, 'still usable after buffers')

  [info.store, info.mirror, *named.values, *opened, empty, fork, shared].each(&:close)
end

def stats_and_report
  s = Store.open('/stats')
  put(s, 'b', '12')
  put(s, 'a', '1')
  put(s, 'a', '123')
  put(s, 'c', 'x', EntryKind::ENCRYPTED)
  expected = Kvstore::Stats.new(entries: 3, bytes: 6,
                                by_kind: { EntryKind::PERSISTENT => 2, EntryKind::ENCRYPTED => 1 })
  expect(Kvstore.summarize(s) == expected, 'summarize')
  key_not_found('q', 'summarize a prefix that matches nothing') { Kvstore.summarize(s, 'q') }
  lines = Kvstore.render_report(s.entries.to_a)
  expect(lines == ['a: 3 bytes, Persistent, v2', 'b: 2 bytes, Persistent', 'c: 1 bytes, Encrypted'],
         "render_report (got #{lines})")
  e = expect_raise(Kvstore::ReportError::NothingToReport, 'render_report([])') { Kvstore.render_report([]) }
  expect(e.is_a?(Kvstore::ReportError) && e.code == 2001 && e.message == 'nothing to report', 'NothingToReport')
  s.close
end

run_and_check_leaks(Kvstore) do
  main = Thread.current
  expect(Kvstore::ABI_VERSION == 4, 'bindings target ABI revision 4')
  constructors
  basics
  iterators
  listeners(main)
  policies
  loaders
  async_calls(main)
  object_graph
  stats_and_report
  expect(callbacks.zero?, 'every callback implementation was released')
end

puts 'ruby/kvstore: OK'
