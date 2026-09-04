// Conformance consumer: kvstore sample, Dart target.
//
// Full-surface drive of the generated dart:ffi wrapper: the Store interface
// (fallible `Store.open` named factory, instance methods passing the object
// pointer, the `defaultCapacity` static, the deprecated `legacyPut`), the
// typed KvException hierarchy (KeyNotFoundException = 1001,
// ExpiredException = 1002, IoException = 1004) thrown by throwing members,
// optional record returns (`Entry?`) decoded from value buffers with bytes /
// optional-scalar / list / map fields, the plain-value Entry class, the
// iterator-backed `listKeys` method, the cross-module `getStats`, the
// consumer-implemented `EvictionListener` callback interface (set, replaced,
// detached by returning false, cleared, and throwing so the caller sees the
// foreign error code -4), the object graph (`share()` as a second wrapper to
// the same native object, `fork()`, `Store?` in and out of `larger`, the
// `StoreInfo` record carrying a `Store` field plus an optional `Store`,
// `openMany` returning a list of objects, `totalCount` taking a list of
// objects and a Dart-built record holding objects), the Future-returning
// `compact`, and dispose / double dispose / use after dispose on every
// wrapper. Throws (non-zero exit) on any mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

/// Records every eviction; returns false (detaching itself) once it has seen
/// `keepFor` evictions, and throws when `fail` is set.
class RecordingListener extends wv.EvictionListener {
  RecordingListener(this.name, {this.keepFor = 1 << 30, this.fail = false});

  final String name;
  final int keepFor;
  final bool fail;
  final List<(wv.Entry, wv.EvictionReason)> evictions =
      <(wv.Entry, wv.EvictionReason)>[];

  @override
  bool onEvict(wv.Entry entry, wv.EvictionReason reason) {
    if (fail) throw StateError('$name refuses eviction of ${entry.key}');
    evictions.add((entry, reason));
    return evictions.length < keepFor;
  }

  List<String> get keys => evictions.map((e) => e.$1.key).toList();
}

Future<void> main() async {
  // Fallible constructor: an empty path reports the IoError domain code
  // through the typed exception hierarchy.
  try {
    wv.Store.open('');
    throw StateError('expected IoException for empty path');
  } on wv.IoException catch (e) {
    expect(e.code == 1004, 'IoError code == 1004 (got ${e.code})');
    expect(e.message == 'I/O failure', 'IoError message (got ${e.message})');
    expect(e is wv.KvException, 'IoException extends KvException');
    expect(e is wv.WeaveFFIException, 'IoException extends the generic brand');
  }

  final store = wv.Store.open('/tmp/conformance-kvstore-dart');

  // Static method on the interface.
  expect(wv.Store.defaultCapacity() == 1000000, 'default capacity');

  final payload = <int>[1, 2, 3];
  expect(store.put('alpha', payload, wv.EntryKind.persistent, null),
      'put alpha');
  expect(store.put('beta', payload, wv.EntryKind.volatile, 3600), 'put beta');
  expect(store.count() == 2, 'count == 2');

  // Iterator-backed list-of-string method, with and without the prefix; the
  // producer yields sorted keys, and each iteration is a fresh native
  // iterator.
  final keys = store.listKeys(null).toList();
  expect(keys.length == 2 && keys[0] == 'alpha' && keys[1] == 'beta',
      'listKeys values (got $keys)');
  final filtered = store.listKeys('al').toList();
  expect(filtered.length == 1 && filtered[0] == 'alpha',
      'listKeys prefix filter');
  expect(store.listKeys('zzz').isEmpty, 'listKeys no match');
  expect(store.listKeys(null).first == 'alpha', 'abandoned partial iteration');

  // Optional record return: the `Entry?` buffer decodes into a plain value
  // class covering every complex field type.
  final alpha = store.get('alpha')!;
  expect(alpha.id > 0, 'entry id positive');
  expect(alpha.key == 'alpha', 'entry key');
  final value = alpha.value;
  expect(value.length == 3 && value[0] == 1 && value[2] == 3, 'entry value');
  expect(alpha.createdAt > 0, 'entry createdAt positive');
  expect(alpha.expiresAt == null, 'entry expiresAt null');
  expect(alpha.tags.isEmpty, 'entry tags empty');
  expect(alpha.metadata.isEmpty, 'entry metadata empty');

  final beta = store.get('beta')!;
  expect(beta.expiresAt != null && beta.expiresAt! > beta.createdAt,
      'beta expiresAt set');
  expect(beta.id == alpha.id + 1, 'ids are monotonic');

  // Typed error: a missing key throws the KeyNotFoundException class of the
  // KvException domain, carrying its stable code and default message.
  try {
    store.get('missing');
    throw StateError('expected KeyNotFoundException for missing key');
  } on wv.KeyNotFoundException catch (e) {
    expect(e.code == 1001, 'KeyNotFound code == 1001 (got ${e.code})');
    expect(e.message == 'key not found', 'KeyNotFound message');
    expect(e is wv.KvException, 'KeyNotFound extends KvException');
  }

  // Deprecated method still works.
  expect(store.legacyPut('legacy', payload), 'legacy put');
  expect(store.count() == 3, 'count == 3 after legacy put');
  expect(store.delete('legacy'), 'delete legacy');
  expect(!store.delete('legacy'), 'second delete returns false');

  // Record construction: scalars, bytes, optional, list, and map fields on
  // the plain-value Entry class.
  final entry = wv.Entry(
    id: 7,
    key: 'built',
    value: payload,
    createdAt: 1000,
    expiresAt: null,
    tags: <String>['hot', 'fast'],
    metadata: <String, String>{'source': 'test', 'env': 'prod'},
  );
  expect(entry.id == 7, 'entry id == 7');
  expect(entry.expiresAt == null, 'entry expiresAt null');
  final tags = entry.tags..sort();
  expect(tags.length == 2 && tags[0] == 'fast' && tags[1] == 'hot',
      'entry tags');
  final md = entry.metadata;
  expect(md.length == 2 && md['source'] == 'test' && md['env'] == 'prod',
      'entry metadata');

  // Cross-module call: getStats lives in kv.stats and takes the parent
  // module's Store interface as a parameter.
  final st = wv.getStats(store);
  expect(st.totalEntries == 2, 'stats total entries == 2');
  expect(st.totalBytes == 6, 'stats total bytes == 6 (got ${st.totalBytes})');
  expect(st.expiredEntries == 0, 'stats expired entries == 0');

  // ── Eviction listener (callback interface) ──
  // delete fires onEvict synchronously on the calling thread with the full
  // Entry record and the Deleted reason.
  final listener = RecordingListener('first');
  store.setEvictionListener(listener);
  expect(store.delete('beta'), 'delete beta');
  expect(listener.evictions.length == 1, 'one eviction (got ${listener.keys})');
  final (evictedBeta, betaReason) = listener.evictions.single;
  expect(betaReason == wv.EvictionReason.deleted, 'reason deleted');
  expect(evictedBeta.key == 'beta', 'evicted key');
  expect(evictedBeta.id == beta.id, 'evicted id matches the stored entry');
  expect(evictedBeta.value.length == 3 && evictedBeta.value[1] == 2,
      'evicted value bytes');
  expect(evictedBeta.expiresAt == beta.expiresAt, 'evicted expiresAt');
  expect(evictedBeta.tags.isEmpty && evictedBeta.metadata.isEmpty,
      'evicted empty list and map');

  // An expired entry is evicted on read with the Expired reason, and the
  // read itself reports ExpiredException (1002).
  expect(store.put('expiring', <int>[9], wv.EntryKind.volatile, -1),
      'put expiring');
  try {
    store.get('expiring');
    throw StateError('expected ExpiredException');
  } on wv.ExpiredException catch (e) {
    expect(e.code == 1002, 'Expired code == 1002 (got ${e.code})');
  }
  expect(listener.evictions.length == 2, 'expiry evicted');
  expect(listener.evictions.last.$1.key == 'expiring' &&
          listener.evictions.last.$2 == wv.EvictionReason.expired,
      'expiry eviction details');
  expect(store.count() == 1, 'only alpha remains');

  // Replacing the listener: the new one receives, the old one does not.
  final replacement = RecordingListener('replacement');
  store.setEvictionListener(replacement);
  expect(store.put('r1', payload, wv.EntryKind.persistent, null), 'put r1');
  expect(store.delete('r1'), 'delete r1');
  expect(listener.evictions.length == 2, 'replaced listener is silent');
  expect(replacement.keys.join(',') == 'r1', 'replacement received r1');

  // Clearing the listener stops notifications.
  store.clearEvictionListener();
  expect(store.put('r2', payload, wv.EntryKind.persistent, null), 'put r2');
  expect(store.delete('r2'), 'delete r2');
  expect(replacement.evictions.length == 1, 'cleared listener is silent');
  store.clearEvictionListener();

  // Returning false detaches the listener after that eviction.
  final brief = RecordingListener('brief', keepFor: 2);
  store.setEvictionListener(brief);
  for (final k in <String>['d1', 'd2', 'd3']) {
    expect(store.put(k, payload, wv.EntryKind.persistent, null), 'put $k');
    expect(store.delete(k), 'delete $k');
  }
  expect(brief.keys.join(',') == 'd1,d2',
      'listener detached after returning false (got ${brief.keys})');

  // A throwing listener: the Dart exception surfaces to the caller of
  // `delete` as the generic exception with the foreign code (-4), not as a
  // KvException; the entry was already removed, the VM keeps running, and
  // the listener stays attached.
  final hostile = RecordingListener('hostile', fail: true);
  store.setEvictionListener(hostile);
  expect(store.put('doomed', payload, wv.EntryKind.persistent, null),
      'put doomed');
  try {
    store.delete('doomed');
    throw StateError('expected WeaveFFIException from throwing listener');
  } on wv.WeaveFFIException catch (e) {
    expect(e is! wv.KvException, 'foreign error is not a domain error');
    expect(e.code == wv.WeaveFFIException.foreignCode && e.code == -4,
        'foreign code -4 (got ${e.code})');
    expect(e.message.contains('hostile refuses eviction of doomed'),
        'foreign message carries the exception text (got ${e.message})');
  }
  expect(store.count() == 1, 'entry removed despite the listener failing');
  expect(store.put('doomed2', payload, wv.EntryKind.persistent, null),
      'put doomed2');
  try {
    store.delete('doomed2');
    throw StateError('expected a second foreign error');
  } on wv.WeaveFFIException catch (e) {
    expect(e.code == -4, 'listener still attached after failing');
  }
  store.clearEvictionListener();
  expect(store.put('calm', payload, wv.EntryKind.persistent, null), 'put calm');
  expect(store.delete('calm'), 'delete after clearing the hostile listener');

  // ── Object graph ──
  // share() returns a second wrapper to the SAME native object: a mutation
  // through one is visible through the other, and disposing one (twice) leaves
  // the other usable.
  final shared = store.share();
  expect(shared.count() == store.count(), 'share sees the same count');
  expect(shared.put('via-share', <int>[4, 4], wv.EntryKind.persistent, null),
      'put through share');
  expect(store.get('via-share')!.value.length == 2,
      'mutation through share visible in the original');
  expect(store.put('via-orig', <int>[5], wv.EntryKind.persistent, null),
      'put through original');
  expect(shared.get('via-orig')!.value.single == 5,
      'mutation through original visible in share');
  expect(store.count() == 3 && shared.count() == 3, 'counts agree');
  shared.dispose();
  shared.dispose();
  try {
    shared.count();
    throw StateError('expected StateError after dispose');
  } on StateError catch (e) {
    expect(e.message.contains('dispose'), 'use after dispose message');
  }
  expect(store.count() == 3, 'original alive after disposing its share');

  // fork() is a distinct object: a copy that diverges.
  final forked = store.fork();
  expect(forked.count() == 3, 'fork copies live entries');
  expect(forked.put('only-fork', payload, wv.EntryKind.persistent, null),
      'put into fork');
  expect(forked.count() == 4 && store.count() == 3, 'fork diverged');
  try {
    store.get('only-fork');
    throw StateError('fork mutation leaked into the original');
  } on wv.KeyNotFoundException catch (_) {}

  // larger(): `Store?` both ways.
  final empty = wv.Store.open('/tmp/conformance-kvstore-dart-empty');
  expect(empty.larger(null) == null, 'empty.larger(null) is null');
  final self = store.larger(null);
  expect(self != null, 'store.larger(null) present');
  expect(self!.count() == 3, 'larger(null) is the receiver');
  expect(self.put('via-larger', payload, wv.EntryKind.persistent, null),
      'put through larger(null)');
  expect(store.count() == 4, 'larger(null) aliases the receiver');
  self.dispose();
  final bigger = empty.larger(forked);
  expect(bigger != null && bigger.count() == 4, 'empty.larger(fork) is fork');
  expect(bigger!.put('via-bigger', payload, wv.EntryKind.persistent, null),
      'put through larger(other)');
  expect(forked.count() == 5, 'larger(other) aliases other');
  bigger.dispose();
  final own = forked.larger(empty);
  expect(own != null && own.count() == 5, 'fork.larger(empty) is fork');
  own!.dispose();
  own.dispose();

  // describe(): a record carrying the object itself plus an optional object.
  final info = store.describe('primary', null);
  expect(info.label == 'primary', 'describe label');
  expect(info.count == 4, 'describe count (got ${info.count})');
  expect(info.mirror == null, 'describe mirror absent');
  expect(info.store.count() == 4, 'describe store usable');
  expect(info.store.put('via-info', payload, wv.EntryKind.persistent, null),
      'put through the record object');
  expect(store.count() == 5, 'record object aliases the store');
  final mirrored = store.describe('with-mirror', empty);
  expect(mirrored.mirror != null, 'describe mirror present');
  expect(mirrored.mirror!.count() == 0, 'mirror is the empty store');
  expect(mirrored.mirror!.put('m', payload, wv.EntryKind.persistent, null),
      'put through mirror');
  expect(empty.count() == 1, 'mirror aliases the empty store');
  expect(mirrored.store.count() == 5 && mirrored.count == 5,
      'mirrored describe snapshot');

  // openMany(): a list of objects as a return; the error path is typed.
  final many = wv.Store.openMany(<String>['/tmp/many-a', '/tmp/many-b']);
  expect(many.length == 2, 'openMany returns 2 stores');
  expect(many[0].count() == 0 && many[1].count() == 0, 'fresh stores');
  expect(many[0].put('m0', payload, wv.EntryKind.persistent, null), 'put m0');
  expect(many[0].count() == 1 && many[1].count() == 0,
      'openMany stores are distinct');
  expect(wv.Store.openMany(<String>[]).isEmpty, 'openMany empty list');
  try {
    wv.Store.openMany(<String>['/tmp/ok', '']);
    throw StateError('expected IoException from openMany');
  } on wv.IoException catch (e) {
    expect(e.code == 1004, 'openMany IoError code');
  }

  // totalCount(): objects inside a list parameter and inside an optional
  // record parameter (each written as a cloned token, so the wrappers stay
  // valid afterward). The record is built in Dart from live wrappers.
  expect(wv.Store.totalCount(<wv.Store>[], null) == 0, 'totalCount of nothing');
  expect(wv.Store.totalCount(many, null) == 1, 'totalCount of many');
  expect(wv.Store.totalCount(<wv.Store>[store, forked, empty], null) == 11,
      'totalCount of three stores');
  expect(wv.Store.totalCount(<wv.Store>[], info) == 5,
      'totalCount of a record only');
  expect(wv.Store.totalCount(many, mirrored) == 6, 'totalCount list + record');
  final built = wv.StoreInfo(
      label: 'built', store: forked, mirror: empty, count: -1);
  expect(wv.Store.totalCount(many, built) == 6,
      'totalCount with a Dart-built record holding objects');
  final builtNoMirror = wv.StoreInfo(label: 'b2', store: many[0], count: 0);
  expect(wv.Store.totalCount(<wv.Store>[many[0]], builtNoMirror) == 2,
      'totalCount counts the same object twice');
  expect(store.count() == 5 && forked.count() == 5 && empty.count() == 1,
      'wrappers untouched after being encoded into buffers');

  // Encoding a disposed wrapper into a buffer is a Dart-side StateError, not
  // a native fault.
  final gone = store.share();
  gone.dispose();
  try {
    wv.Store.totalCount(<wv.Store>[gone], null);
    throw StateError('expected StateError encoding a disposed wrapper');
  } on StateError catch (e) {
    expect(e.message.contains('dispose'), 'disposed wrapper in a buffer');
  }

  // Release every object from the graph; the store itself must survive all
  // of them because it still holds its own reference.
  info.store.dispose();
  mirrored.store.dispose();
  mirrored.mirror!.dispose();
  mirrored.mirror!.dispose();
  for (final s in many) {
    s.dispose();
  }
  forked.dispose();
  empty.dispose();
  expect(store.count() == 5, 'store alive after releasing the graph');

  // Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
  // Future settles via a NativeCallable.listener message from the producer's
  // worker thread.
  expect(store.put('doomed', payload, wv.EntryKind.volatile, 0), 'put doomed');
  final reclaimed = await store.compact();
  expect(reclaimed == 3, 'compact reclaimed 3 bytes (got $reclaimed)');
  expect(store.count() == 5, 'live entries untouched by compact');
  expect((await store.compact()) == 0, 'second compact reclaims nothing');

  store.clear();
  expect(store.count() == 0, 'clear empties the store');

  store.dispose();
  store.dispose();
  try {
    store.count();
    throw StateError('expected StateError after dispose');
  } on StateError catch (_) {}

  print('dart/kvstore: OK');
}
