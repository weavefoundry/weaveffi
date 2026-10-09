// Conformance consumer: kvstore sample, Dart target.
//
// Drives the feature-complete producer through the generated bindings:
//
//   * the load-time checks (ABI revision, both modules' contract tables),
//     which loading the bindings performs;
//   * `Store`: fallible and infallible constructors (`Store.open`, `Store()`),
//     methods, statics, the deprecated `size()`, the `Entry` and `StoreInfo`
//     value classes, the `EntryKind` enum, maps and optionals, the logical
//     clock, an optional TTL parameter and an optional expiry return (optional
//     scalars crossing directly), a `[u64]` return (a typed array), `usize`
//     counts, and a `throws any` method (`importLines`);
//   * the open `KvException` hierarchy with its payload fields
//     (`KeyNotFound`, `Expired`, `StoreFull`, `Rejected`, `CallbackFailed`);
//   * lazy `Iterable`s of strings (throwing at launch), records, objects, and
//     optional scalars, including one abandoned part-way;
//   * four callback interfaces implemented in Dart: a `Listener` (retained,
//     filtered by `accepts`, told about every `Change` on the event loop,
//     detached when it throws, and detached when the producer calls its
//     value-returning `accepts` from a worker thread, which the thread-affine
//     vtable refuses instead of letting the VM abort), a `Policy` (an
//     optional-scalar method choosing the TTL, a record return, a typed
//     `RejectedException` that reaches the `put` caller with its payload, any
//     other failure arriving as `CallbackFailed`, an object parameter and
//     object return, and calls back into the store from inside a callback,
//     each in its own call frame), a `Loader` passed as an optional callback
//     (string, bytes, and optional-object returns; typed errors decoded by the
//     producer or passed through), and a `Scorer` (a typed array in and out);
//   * `Store` objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * futures: an async free function returning an object, a cancellable
//     method cancelled mid-pause (completing at once with
//     `CancelledException` while its background work stops cooperatively,
//     shown by `activeJobs`), an async list launched concurrently, async
//     optional-scalar and typed-array results, and an async function in the
//     nested `kv.stats` module;
//   * the sibling `report` root (the shared `Entry` record and its own error
//     domain).
//
// Releases of consumer callbacks are observed through the producer's
// callback counter (`debug_live(1)`). Ends by asserting the producer's leak
// counters are zero.

import 'dart:convert';

import 'package:kvstore/kvstore.dart';

import 'support.dart';

final int Function(int) live = debugLive('kvstore');

/// Live consumer callbacks the producer holds.
int liveCallbacks() => live(1);

String text(List<int> bytes) => utf8.decode(bytes);

extension on Store {
  Entry putText(
    String key,
    String value, [
    EntryKind kind = EntryKind.persistent,
    int? ttl,
  ]) => put(key, utf8.encode(value), kind, ttl);
}

/// Whether [a] and [b] wrap the same native store: a write through one is
/// visible through the other (distinct stores never share entries).
bool sameStore(Store a, Store b) {
  const probe = '__identity_probe__';
  a.putText(probe, 'x');
  final same = b.find(probe) != null;
  a.delete(probe);
  return same;
}

void expectKeyNotFound(String key, void Function() body) {
  final e = expectThrows<KeyNotFoundException>(body, 'key $key');
  expect(
    e.code == 1001 && e.key == key,
    'KeyNotFound payload key $key (got ${e.key})',
  );
}

bool listEquals<T>(List<T> a, List<T> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// Churns garbage until [done] holds, so finalizers run.
Future<void> collectUntil(bool Function() done, String msg) async {
  final watch = Stopwatch()..start();
  var junk = <Object>[];
  while (!done()) {
    if (watch.elapsed > const Duration(seconds: 10))
      throw StateError('timed out: $msg');
    for (var i = 0; i < 20000; i++) {
      junk.add(List<int>.filled(64, i));
    }
    junk = <Object>[];
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

// ── listener (consumer-implemented, retained) ─────────────────────────────

class RecordingListener implements Listener {
  RecordingListener({this.skip, this.failOn});

  final String? skip;
  final String? failOn;
  final List<Change> changes = <Change>[];
  int asked = 0;

  @override
  bool accepts(String key) {
    asked++;
    if (key == failOn) throw StateError('listener refused');
    return key != skip;
  }

  @override
  void onChange(Change change) => changes.add(change);

  List<ChangePut> get puts => changes.whereType<ChangePut>().toList();
}

// ── policy (consumer-implemented, rich returns, throws) ───────────────────

/// An exception whose text is exactly its message.
class Failure implements Exception {
  Failure(this.message);

  final String message;

  @override
  String toString() => message;
}

/// Routes `b/` keys to [other], a reference this policy owns.
class TestPolicy implements Policy {
  TestPolicy(this.other);

  final Store other;
  int admitted = 0;

  @override
  int? ttlFor(String key, int? requested) => requested;

  @override
  Entry admit(Entry entry) {
    admitted++;
    expect(entry.version == 0, 'the store assigns the version after admission');
    // Calls back into the library from inside a callback run in frames of
    // their own: a string return, and a failure caught here, leave the
    // outer `put` untouched.
    expect(other.path() == '/other', 'a nested call returns its own string');
    expectKeyNotFound('nested', () => other.get('nested'));
    if (entry.key.startsWith('secret')) {
      throw RejectedException(
        entry.key,
        'no secrets',
        'secrets are not stored',
      );
    }
    if (entry.key.startsWith('boom')) throw Failure('policy exploded');
    // Tag it and store it encrypted (and try to rename it, which the store
    // ignores).
    return Entry(
      key: 'renamed',
      value: entry.value,
      kind: EntryKind.encrypted,
      version: entry.version,
      expiresAt: entry.expiresAt,
      tags: const ['admitted'],
      metadata: entry.metadata,
    );
  }

  @override
  Store route(String key, Store home) {
    final target = key.startsWith('b/') ? other : home;
    // The producer gets its own reference; `home` was ours to release.
    if (!identical(target, home)) home.dispose();
    return target;
  }
}

/// Picks TTLs by key: `short` gets 1, `forever` none, `full` and `oops`
/// fail (typed and untyped), and anything else keeps the requested TTL.
class TtlPolicy implements Policy {
  @override
  int? ttlFor(String key, int? requested) => switch (key) {
    'short' => 1,
    'forever' => null,
    'full' => throw StoreFullException(7),
    'oops' => throw Failure('ttl exploded'),
    _ => requested,
  };

  @override
  Entry admit(Entry entry) => entry;

  @override
  Store route(String key, Store home) => home;
}

// ── scorer (consumer-implemented, typed arrays in and out) ───────────────

class TestScorer implements Scorer {
  TestScorer({this.fail = false, this.drop = false});

  final bool fail;
  final bool drop;
  List<int>? seen;

  @override
  List<double> scores(List<int> sizes) {
    seen = sizes.toList();
    if (fail) throw Failure('scorer is broken');
    final scores = [for (final size in sizes) size * 1.0];
    return drop ? scores.sublist(1) : scores;
  }
}

// ── loader (consumer-implemented, passed as an optional parameter) ────────

class TestLoader implements Loader {
  TestLoader([this.backup]);

  final Store? backup;

  @override
  String name() => 'dart-loader';

  @override
  Store? fallback(String key) => key == 'fb' ? backup : null;

  @override
  List<int> load(String key) => switch (key) {
    'missing' => throw KeyNotFoundException('missing', 'not in the loader'),
    'elsewhere' => throw KeyNotFoundException('other', 'not in the loader'),
    'broken' => throw Failure('loader is broken'),
    _ => utf8.encode('loaded:$key'),
  };
}

// ── sections ──────────────────────────────────────────────────────────────

Future<void> constructors() async {
  final e = expectThrows<InvalidPathException>(
    () => Store.open(''),
    'Store.open("")',
  );
  expect(
    e.code == 1004 && e.message == 'invalid path',
    'InvalidPath code and message',
  );

  final s = Store();
  expect(s.path() == 'memory', 'Store() path');
  expect(
    s.capacity() == Store.defaultCapacity(),
    'capacity == defaultCapacity',
  );
  expect(Store.defaultCapacity() == 1000000, 'defaultCapacity == 1000000');
  s.dispose();

  final opened = await openStore('/async');
  expect(opened.path() == '/async', 'openStore path');
  opened.dispose();
  await expectThrowsAsync<InvalidPathException>(
    () => openStore(''),
    'openStore("") rejects with InvalidPath',
  );
}

void basics() {
  final s = Store.open('/basics');
  // put returns the stored entry; the version counts puts of the key.
  final e1 = s.putText('alpha', 'one', EntryKind.persistent);
  expect(
    e1.key == 'alpha' &&
        text(e1.value) == 'one' &&
        e1.kind == EntryKind.persistent,
    'put alpha',
  );
  expect(
    e1 ==
        Entry(
          key: 'alpha',
          value: utf8.encode('one'),
          kind: EntryKind.persistent,
          version: 1,
          tags: const [],
          metadata: const {},
        ),
    'put alpha fields (got $e1)',
  );
  final e2 = s.putText('alpha', 'two', EntryKind.volatile);
  expect(e2.version == 2 && e2.kind == EntryKind.volatile, 'put alpha again');

  expect(text(s.get('alpha').value) == 'two', 'get alpha');
  final missing = expectThrows<KeyNotFoundException>(
    () => s.get('nope'),
    'get(nope)',
  );
  expect(
    missing.key == 'nope' && missing.message == 'key not found: nope',
    'KeyNotFound(nope)',
  );
  expect(s.find('alpha')?.version == 2, 'find alpha');
  expect(s.find('nope') == null, 'find nope');

  // TTLs follow the logical clock; an expired get reports when.
  expect(
    s.putText('ttl', 'x', EntryKind.volatile, 10).expiresAt == 10,
    'ttl expiresAt',
  );
  expect(s.now() == 0, 'clock starts at 0');
  expect(s.tick(9) == 9 && s.count() == 2, 'tick 9');
  expect(s.tick(1) == 10 && s.count() == 1, 'tick 10');
  final expired = expectThrows<ExpiredException>(
    () => s.get('ttl'),
    'get(ttl)',
  );
  expect(expired.key == 'ttl' && expired.expiredAt == 10, 'Expired payload');
  expect(
    expired.message == 'entry ttl expired at 10',
    'Expired message (got ${expired.message})',
  );
  expectKeyNotFound('ttl', () => s.get('ttl')); // the expired read removed it

  // Capacity: a new key past it is StoreFull { capacity }.
  s.setCapacity(1);
  expect(s.capacity() == 1, 'setCapacity');
  s.putText('alpha', 'three'); // replacing is fine
  final full = expectThrows<StoreFullException>(
    () => s.putText('beta', 'b'),
    'put beta at capacity',
  );
  expect(full.capacity == 1, 'StoreFull capacity');
  s.setCapacity(100);

  // delete, clear, and the deprecated size().
  s.putText('beta', 'b');
  expect(s.delete('beta') && !s.delete('beta'), 'delete twice');
  // ignore: deprecated_member_use
  final size = s.size();
  expect(size == s.count() && size == 1, 'deprecated size() == count()');
  expect(s.clear() == 1 && s.count() == 0, 'clear');
  s.dispose();
}

Future<void> iterators() async {
  final s = Store.open('/iter');
  s.putText('user.bob', 'b');
  s.putText('user.alice', 'a');
  s.putText('sys.x', 'xx');

  expect(
    listEquals(s.keys(null).toList(), ['sys.x', 'user.alice', 'user.bob']),
    'keys in order',
  );
  expect(live(2) == 0, 'an exhausted iterator is released');
  expectKeyNotFound('zzz', () => s.keys('zzz').toList());

  // Abandoning an iterator part-way releases it (through its finalizer).
  void abandon() {
    final keys = s.keys('user.').iterator;
    expect(keys.moveNext() && keys.current == 'user.alice', 'first user key');
    expect(live(2) == 1, 'the iterator is live');
  }

  abandon();
  await collectUntil(() => live(2) == 0, 'an abandoned iterator is released');

  final sys = s.entries('sys.').toList();
  expect(
    sys.length == 1 && sys[0].key == 'sys.x' && text(sys[0].value) == 'xx',
    'entries(sys.)',
  );

  // partition: objects, created as they're pulled.
  const prefixes = ['user.', 'sys.', 'none.'];
  const counts = [2, 1, 0];
  var i = 0;
  for (final part in s.partition(prefixes)) {
    expect(!sameStore(part, s), 'partition yields new stores');
    expect(
      part.count() == counts[i] && part.path() == prefixes[i],
      'partition $i',
    );
    part.dispose();
    i++;
  }
  expect(i == 3, 'three partitions');
  s.dispose();
}

Future<void> listeners() async {
  final base = liveCallbacks();
  final s = Store.open('/listen');
  final l = RecordingListener(skip: 'quiet');
  final id = s.subscribe(l);
  expect(
    id > 0 && s.listenerCount() == 1 && liveCallbacks() == base + 1,
    'subscribe',
  );

  // Void methods are delivered on the event loop, after the call returns.
  s.putText('a', '1');
  await settle(() => l.puts.length == 1, 'Put v1 delivered');
  var put = l.puts.last;
  expect(put.entry.version == 1 && !put.replaced, 'Put v1');
  s.putText('a', '2');
  await settle(() => l.puts.length == 2, 'Put v2 delivered');
  put = l.puts.last;
  expect(
    put.entry.version == 2 && put.replaced && put.entry.key == 'a',
    'Put v2',
  );
  s.putText('quiet', 'x'); // accepts() said no
  expect(s.delete('a'), 'delete a');
  await settle(() => l.changes.length == 3, 'Removed delivered');
  expect(l.puts.length == 2, 'the filter skipped quiet');
  expect(
    l.changes.last == ChangeRemoved('a', false),
    'Removed(a) (got ${l.changes.last})',
  );

  // An expired read removes the entry and says so.
  s.putText('short', 'x', EntryKind.volatile, 1);
  s.tick(1);
  expectThrows<ExpiredException>(() => s.get('short'), 'get(short)');
  await settle(() => l.changes.length == 5, 'Removed(short) delivered');
  expect(
    l.changes.last == ChangeRemoved('short', true),
    'Removed(short, expired)',
  );

  expect(s.clear() == 1, 'clear leaves quiet'); // "quiet" was left
  await settle(() => l.changes.length == 6, 'Cleared delivered');
  expect(l.changes.last == ChangeCleared(1), 'Cleared(1)');

  // Unsubscribing releases the listener once.
  expect(
    s.unsubscribe(id) && liveCallbacks() == base,
    'unsubscribe releases the listener',
  );
  expect(!s.unsubscribe(id) && s.listenerCount() == 0, 'unsubscribe twice');

  // A listener that fails is detached (and released); the put succeeds.
  final failing = RecordingListener(failOn: 'boom');
  s.subscribe(failing);
  s.putText('fine', '1');
  s.putText('boom', '1');
  expect(
    s.count() == 2 && s.listenerCount() == 0,
    'a failing listener is detached',
  );
  expect(liveCallbacks() == base, 'the failing listener was released');
  await settle(() => failing.puts.isNotEmpty, 'the failing listener saw fine');
  expect(
    failing.puts.single.entry.key == 'fine',
    'the failing listener saw only fine',
  );

  // Disposing the store releases the listeners it still holds.
  s.subscribe(RecordingListener());
  s.subscribe(RecordingListener());
  expect(
    s.listenerCount() == 2 && liveCallbacks() == base + 2,
    'two listeners',
  );
  s.dispose();
  expect(liveCallbacks() == base, 'disposing the store released its listeners');
}

void policies() {
  final base = liveCallbacks();
  final s = Store.open('/policy');
  final other = Store.open('/other');
  final p = TestPolicy(other.share());
  s.setPolicy(p);
  expect(s.hasPolicy() && liveCallbacks() == base + 1, 'setPolicy');

  // admit's record return is what's stored (its key and version aside).
  final a = s.putText('a', '1', EntryKind.volatile);
  expect(
    a.key == 'a' && a.version == 1 && a.kind == EntryKind.encrypted,
    'admitted entry',
  );
  expect(listEquals(a.tags, ['admitted']), "admit's rewrite");

  // route: the object parameter and object return redirect a write.
  s.putText('b/x', '2', EntryKind.volatile);
  expect(s.count() == 1 && other.count() == 1, 'route redirected b/x');

  // A typed error from the throwing callback reaches the caller with its
  // code and payload, and the message the producer renders from them.
  final rejected = expectThrows<RejectedException>(
    () => s.putText('secret', '3'),
    'put(secret)',
  );
  expect(
    rejected.code == 1005 &&
        rejected.message == 'write to secret rejected: no secrets',
    'Rejected code and message (got ${rejected.code}: ${rejected.message})',
  );
  expect(
    rejected.key == 'secret' && rejected.reason == 'no secrets',
    'Rejected payload',
  );

  // Anything else arrives as CallbackFailed with the consumer's message.
  final boom = expectThrows<CallbackFailedException>(
    () => s.putText('boom', '4'),
    'put(boom)',
  );
  expect(
    boom.code == 1006 &&
        boom.message_ == 'policy exploded' &&
        boom.message == 'policy exploded',
    'CallbackFailed (got ${boom.code}: ${boom.message})',
  );
  expect(
    s.count() == 1 && other.count() == 1 && p.admitted == 4,
    'failed puts changed nothing',
  );

  // Replacing the policy releases the old one; null removes it.
  final replacement = TestPolicy(other.share());
  s.setPolicy(replacement);
  expect(
    liveCallbacks() == base + 1,
    'replacing the policy released the old one',
  );
  s.setPolicy(null);
  expect(
    !s.hasPolicy() && liveCallbacks() == base,
    'setPolicy(null) released it',
  );
  s.putText('secret', 'now allowed');
  expect(s.count() == 2, 'no policy, no veto');

  p.other.dispose();
  replacement.other.dispose();
  other.dispose();
  s.dispose();

  // ttl_for: an optional scalar in and out picks each write's TTL.
  final t = Store.open('/ttl');
  t.setPolicy(TtlPolicy());
  expect(t.putText('short', 'x').expiresAt == 1, 'ttlFor overrides none');
  expect(
    t.putText('forever', 'x', EntryKind.persistent, 5).expiresAt == null,
    'ttlFor removes a TTL',
  );
  expect(
    t.putText('other', 'x', EntryKind.persistent, 9).expiresAt == 9,
    'ttlFor keeps the requested TTL',
  );
  expect(t.putText('plain', 'x').expiresAt == null, 'no TTL stays none');
  final full = expectThrows<StoreFullException>(
    () => t.putText('full', 'x'),
    'ttlFor raising StoreFull',
  );
  expect(
    full.capacity == 7 && full.message == 'store is full (7 entries)',
    'a typed ttlFor failure reaches put (got ${full.message})',
  );
  final oops = expectThrows<CallbackFailedException>(
    () => t.putText('oops', 'x'),
    'ttlFor failing otherwise',
  );
  expect(oops.message_ == 'ttl exploded', 'CallbackFailed from ttlFor');
  expect(t.count() == 4, 'failed puts stored nothing');
  t.setPolicy(null);
  t.dispose();
}

void loaders() {
  final s = Store.open('/load');
  final base = liveCallbacks();

  // No loader (a null optional callback): a miss is none.
  expect(s.getOrLoad('k', null) == null, 'no loader');

  // load's bytes are stored, tagged with the loader's name.
  final loaded = s.getOrLoad('k', TestLoader());
  expect(loaded != null && text(loaded.value) == 'loaded:k', 'loaded value');
  expect(
    loaded!.kind == EntryKind.volatile &&
        loaded.metadata['source'] == 'dart-loader',
    'loaded entry (got $loaded)',
  );
  expect(liveCallbacks() == base, 'a loader is released after the call');
  expect(s.count() == 1, 'the loaded entry is stored');
  // A hit doesn't consult the loader.
  expect(s.getOrLoad('k', TestLoader())?.version == 1, 'a hit');

  // The fallback store (an optional object return) is consulted first.
  final backup = Store.open('/backup');
  backup.putText('fb', 'from backup');
  final fb = s.getOrLoad('fb', TestLoader(backup));
  expect(
    fb != null &&
        text(fb.value) == 'from backup' &&
        fb.kind == EntryKind.persistent,
    'fallback entry',
  );
  backup.dispose();

  // KeyNotFound for this key: the producer decoded the payload and answers
  // none.
  expect(
    s.getOrLoad('missing', TestLoader()) == null,
    'KeyNotFound for the same key is none',
  );
  // KeyNotFound for another key: passed through, payload intact, with the
  // message the producer renders from it.
  final other = expectThrows<KeyNotFoundException>(
    () => s.getOrLoad('elsewhere', TestLoader()),
    'getOrLoad(elsewhere)',
  );
  expect(
    other.key == 'other' && other.message == 'key not found: other',
    'KeyNotFound(other) passed through',
  );
  // Any other failure is CallbackFailed with the loader's message.
  final broken = expectThrows<CallbackFailedException>(
    () => s.getOrLoad('broken', TestLoader()),
    'getOrLoad(broken)',
  );
  expect(
    broken.code == 1006 && broken.message_ == 'loader is broken',
    'CallbackFailed (got ${broken.code}: ${broken.message})',
  );
  expect(liveCallbacks() == base, 'every loader was released');
  s.dispose();
}

Future<void> asyncCalls() async {
  final base = liveCallbacks();
  final s = Store.open('/async-calls');
  // Subscribed on this thread, so `put` (a synchronous call) asks it here.
  final affine = RecordingListener();
  s.subscribe(affine);
  s.putText('old1', 'x', EntryKind.volatile, 1);
  s.putText('old2', 'x', EntryKind.volatile, 1);
  s.putText('keep', 'x');
  s.tick(5);
  expect(affine.asked == 3, 'accepts ran for each put (got ${affine.asked})');

  // compact runs on a producer thread and tells listeners there. The
  // listener's vtable is thread-affine, so the producer refuses to call its
  // `accepts` (it returns a value) off this thread: the method never runs,
  // the listener fails with -4 and is detached, and compaction carries on.
  final token = CancelToken();
  expect(await s.compact(0, cancelToken: token) == 2, 'compact(0) removed 2');
  expect(s.count() == 1, 'one entry left');
  expect(affine.asked == 3, 'accepts never ran off its thread');
  expect(
    s.listenerCount() == 0 && liveCallbacks() == base,
    'the refused listener was detached and released',
  );
  expect(await s.compact(5) == 0, 'compact(5) removed nothing');

  // Cancel mid-pause: the call completes at once with a cancellation, and
  // the background pause notices the token and stops.
  final cancel = CancelToken();
  var done = false;
  final pending = s
      .compact(60000, cancelToken: cancel)
      .whenComplete(() => done = true);
  await Future<void>.delayed(const Duration(milliseconds: 20));
  expect(!done, 'compact(60000) is pausing');
  expect(Store.activeJobs() >= 1, 'the pause is running');
  final started = Stopwatch()..start();
  cancel.cancel();
  await expectThrowsAsync<CancelledException>(
    () => pending,
    'a cancelled compact',
  );
  expect(
    started.elapsed < const Duration(seconds: 2),
    'cancellation is prompt',
  );
  await settle(
    () => Store.activeJobs() == 0,
    'the cancelled pause stopped cooperatively',
    timeout: const Duration(seconds: 2),
  );

  // Async optional-scalar and typed-array results.
  final v = Store.open('/versions');
  v.putText('b', '12');
  v.putText('a', '123', EntryKind.volatile, 7);
  expect(await v.versionOf('a') == 1, 'versionOf(a)');
  expect(await v.versionOf('q') == null, 'versionOf(q) is absent');
  v.putText('b', 'x');
  final versions = await v.versions(['b', 'q', 'a']);
  expect(listEquals(versions, [2, 0, 1]), 'versions (got $versions)');
  expect(
    listEquals(await v.versions(const []), const <int>[]),
    'versions([])',
  );
  v.dispose();

  // getMany: an async list of optional records, launched concurrently.
  final results = await Future.wait([
    for (var i = 0; i < 32; i++) s.getMany(['keep', 'gone', 'keep']),
  ]);
  for (final got in results) {
    expect(
      got.length == 3 &&
          got[0]?.key == 'keep' &&
          got[1] == null &&
          got[2]?.key == 'keep',
      'getMany',
    );
  }

  // The nested module's async function: objects in a list in, a record out.
  final other = Store.open('/other');
  other.putText('a', '123');
  final st = await summarizeAll([s, other]);
  expect(
    st == Stats(entries: 2, bytes: 4, byKind: {EntryKind.persistent: 2}),
    'summarizeAll (got $st)',
  );
  other.dispose();
  s.dispose();
}

void objectGraph() {
  final s0 = Store.open('/graph');
  s0.putText('k', 'v');

  // share(): the same object; the original can go.
  final s = s0.share();
  expect(sameStore(s, s0), 'share() is the same object');
  s0.dispose();
  expect(s.count() == 1, 'alive through the shared reference');

  // fork(): a distinct object with a copy of the entries.
  final fork = s.fork();
  expect(
    !sameStore(fork, s) && fork.count() == 1 && fork.path() == '/graph',
    'fork',
  );
  fork.putText('k2', 'v');
  expect(fork.count() == 2 && s.count() == 1, 'fork is independent');

  // larger(): `Store?` in and out.
  final empty = Store.open('/empty');
  expect(empty.larger(null) == null, 'larger(null) on an empty store');
  final larger = empty.larger(fork)!;
  expect(sameStore(larger, fork), 'larger(fork) is fork');
  larger.dispose();
  final self = s.larger(null)!;
  expect(sameStore(self, s), 'larger(null) is self');
  self.dispose();

  // describe(): a record whose fields carry objects.
  final info = s.describe('main', fork);
  expect(info.label == 'main' && info.count == 1, 'describe label and count');
  expect(
    sameStore(info.store, s) && sameStore(info.mirror!, fork),
    'describe objects',
  );
  expect(info.mirror?.count() == 2, 'the mirror is live');

  // openMany(): a list of objects; one bad path fails the whole call.
  final many = Store.openMany(['/a', '/b']);
  expect(
    listEquals([for (final m in many) m.path()], ['/a', '/b']),
    'openMany paths',
  );
  expectThrows<InvalidPathException>(
    () => Store.openMany(['/a', '']),
    'openMany with an empty path',
  );

  // byLabel(): records with objects in, a map with object values out.
  final named = Store.byLabel([
    info,
    StoreInfo(label: 'first', store: many[0], count: 0),
  ]);
  expect(
    named.length == 2 &&
        named.containsKey('main') &&
        named.containsKey('first'),
    'byLabel keys',
  );
  expect(sameStore(named['main']!, s), 'byLabel main');
  expect(sameStore(named['first']!, many[0]), 'byLabel first');

  // totalCount(): a list, a map, and an optional record, all carrying
  // objects (each written as a fresh reference the producer adopts).
  many[0].putText('m', '1');
  final stores = [many[0], many[1], fork];
  expect(Store.totalCount(stores, named, info) == 6, 'totalCount with extra');
  expect(
    Store.totalCount(stores, named, null) == 5,
    'totalCount without extra',
  );

  // Everything is still intact; release each reference once.
  expect(
    s.count() == 1 && fork.count() == 2 && many[0].count() == 1,
    'still usable',
  );
  for (final m in named.values) {
    m.dispose();
  }
  info.store.dispose();
  info.mirror?.dispose();
  for (final m in many) {
    m.dispose();
  }
  empty.dispose();
  fork.dispose();
  s.dispose();
  expectThrows<StateError>(() => s.count(), "a disposed wrapper can't be used");
}

void directTransports() {
  final base = liveCallbacks();
  final s = Store.open('/direct');
  s.putText('b', '12');
  s.put('a', [1, 2, 3], EntryKind.volatile, 7);
  expect(s.count() == 2, 'count is a usize');
  expect(s.expiresAt('a') == 7, 'expiresAt(a)');
  expect(s.expiresAt('b') == null, 'expiresAt(b) is absent');
  expect(s.expiresAt('zzz') == null, 'expiresAt(zzz) is absent');
  final sizes = s.valueSizes();
  expect(listEquals(sizes, [3, 2]), 'valueSizes (got $sizes)');
  final expirations = s.expirations().toList();
  expect(
    listEquals(expirations, [7, null]),
    'expirations (got $expirations)',
  );
  s.dispose();

  // rank: the scorer gets a typed array and returns one.
  final r = Store.open('/rank');
  r.putText('a', '1');
  r.putText('b', '333');
  r.putText('c', '22');
  final scorer = TestScorer();
  final ranked = r.rank(scorer);
  expect(listEquals(ranked, ['b', 'c', 'a']), 'rank (got $ranked)');
  expect(listEquals(scorer.seen!, [1, 3, 2]), 'scores(sizes) (got ${scorer.seen})');
  final broken = expectThrows<CallbackFailedException>(
    () => r.rank(TestScorer(fail: true)),
    'rank with a failing scorer',
  );
  expect(
    broken.code == 1006 && broken.message_ == 'scorer is broken',
    'a failing scorer (got ${broken.message})',
  );
  final short = expectThrows<CallbackFailedException>(
    () => r.rank(TestScorer(drop: true)),
    'rank with too few scores',
  );
  expect(
    short.message_ == 'expected 3 scores, got 2',
    'a short score list (got ${short.message})',
  );
  expect(liveCallbacks() == base, 'every scorer was released');
  r.dispose();

  // importLines: `throws any`, failing with -1 and the producer's message.
  final i = Store.open('/import');
  expect(i.importLines('a=1\n\nb=two\n') == 2, 'importLines stored 2');
  expect(text(i.get('b').value) == 'two', 'imported b');
  final bad = expectThrows<NativeException>(
    () => i.importLines('c=3\nbroken\nd=4'),
    'importLines with a malformed line',
  );
  expect(
    bad.runtimeType == NativeException &&
        bad.code == NativeException.genericCode &&
        bad.message == 'line 2: expected key=value',
    'untyped error (got ${bad.runtimeType} ${bad.code}: ${bad.message})',
  );
  expect(i.count() == 3 && i.find('d') == null, 'c was stored, d wasn\'t');
  i.dispose();
}

void statsAndReport() {
  final s = Store.open('/stats');
  s.putText('b', '12');
  s.putText('a', '1');
  s.putText('a', '123');
  s.putText('c', 'x', EntryKind.encrypted);

  // kv.stats: the parent's Store as a parameter, the parent's error domain.
  final st = summarize(s, null);
  expect(
    st ==
        Stats(
          entries: 3,
          bytes: 6,
          byKind: {EntryKind.persistent: 2, EntryKind.encrypted: 1},
        ),
    'summarize (got $st)',
  );
  expectKeyNotFound('q', () => summarize(s, 'q'));

  // report: the sibling root shares the Entry record.
  final lines = renderReport(s.entries(null).toList());
  expect(
    listEquals(lines, [
      'a: 3 bytes, Persistent, v2',
      'b: 2 bytes, Persistent',
      'c: 1 bytes, Encrypted',
    ]),
    'renderReport (got $lines)',
  );
  final nothing = expectThrows<NothingToReportException>(
    () => renderReport(const []),
    'renderReport([])',
  );
  expect(
    nothing.code == 2001 && nothing.message == 'nothing to report',
    'NothingToReport',
  );
  s.dispose();
}

Future<void> main() async {
  await constructors();
  basics();
  await iterators();
  await listeners();
  policies();
  loaders();
  await asyncCalls();
  objectGraph();
  directTransports();
  statsAndReport();

  await expectNoLeaks('kvstore');
  print('dart/kvstore: OK');
}
