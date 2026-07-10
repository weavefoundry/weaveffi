// Conformance consumer: kvstore sample, Dart target.
//
// Full-surface drive of the generated dart:ffi wrapper: the Store interface
// (fallible `Store.open` named factory, instance methods passing the object
// pointer, the `defaultCapacity` static, the deprecated `legacyPut`), the
// typed KvException hierarchy (KeyNotFoundException = 1001,
// IoException = 1004) thrown by throwing members, optional struct returns
// (`Entry?`) with bytes / optional-scalar / list / map getters, the fluent
// EntryBuilder (list + map *input* marshalling), the iterator-backed
// `listKeys` method, the cross-module `getStats` (parameter typed as the
// parent module's Store), the NativeCallable.isolateLocal eviction listener
// (register -> fire synchronously on delete -> unregister), and the
// Future-returning `compact` settled through a NativeCallable.listener from
// the producer's worker thread. Throws (non-zero exit) on any mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

Future<void> main() async {
  // Fallible constructor: an empty path reports the IoError domain code
  // through the typed exception hierarchy.
  try {
    wv.Store.open('');
    throw StateError('expected IoException for empty path');
  } on wv.IoException catch (e) {
    expect(e.code == 1004, 'IoError code == 1004 (got ${e.code})');
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

  // Iterator-backed list-of-string method, with and without the prefix.
  final keys = store.listKeys(null).toList()..sort();
  expect(keys.length == 2 && keys[0] == 'alpha' && keys[1] == 'beta',
      'listKeys values');
  final filtered = store.listKeys('al').toList();
  expect(filtered.length == 1 && filtered[0] == 'alpha',
      'listKeys prefix filter');

  // Optional struct return + getters over every complex field type.
  final alpha = store.get('alpha')!;
  expect(alpha.id > 0, 'entry id positive');
  expect(alpha.key == 'alpha', 'entry key');
  final value = alpha.value;
  expect(value.length == 3 && value[0] == 1 && value[2] == 3, 'entry value');
  expect(alpha.expiresAt == null, 'entry expiresAt null');
  expect(alpha.tags.isEmpty, 'entry tags empty');
  expect(alpha.metadata.isEmpty, 'entry metadata empty');
  alpha.dispose();

  final beta = store.get('beta')!;
  expect(beta.expiresAt != null && beta.expiresAt! > 0, 'beta expiresAt set');
  beta.dispose();

  // Typed error: a missing key throws the KeyNotFoundException class of the
  // KvException domain, carrying its stable code.
  try {
    store.get('missing');
    throw StateError('expected KeyNotFoundException for missing key');
  } on wv.KeyNotFoundException catch (e) {
    expect(e.code == 1001, 'KeyNotFound code == 1001 (got ${e.code})');
    expect(e is wv.KvException, 'KeyNotFound extends KvException');
  }

  // Deprecated method still works.
  expect(store.legacyPut('legacy', payload), 'legacy put');
  expect(store.delete('legacy'), 'delete legacy');

  // Builder input marshaling: scalars, bytes, optional, list, and map.
  final entry = wv.EntryBuilder()
      .withId(7)
      .withKey('built')
      .withValue(payload)
      .withCreatedAt(1000)
      .withExpiresAt(null)
      .withTags(<String>['hot', 'fast'])
      .withMetadata(<String, String>{'source': 'test', 'env': 'prod'})
      .build();
  expect(entry.id == 7, 'entry id == 7');
  expect(entry.expiresAt == null, 'entry expiresAt null');
  final tags = entry.tags..sort();
  expect(tags.length == 2 && tags[0] == 'fast' && tags[1] == 'hot',
      'entry tags');
  final md = entry.metadata;
  expect(md.length == 2 && md['source'] == 'test' && md['env'] == 'prod',
      'entry metadata');
  entry.dispose();

  // Empty list/map round-trip as zero-length.
  final empty = wv.EntryBuilder()
      .withId(8)
      .withKey('k')
      .withValue(payload)
      .withCreatedAt(1000)
      .withExpiresAt(null)
      .withTags(<String>[])
      .withMetadata(<String, String>{})
      .build();
  expect(empty.metadata.isEmpty, 'empty metadata');
  expect(empty.tags.isEmpty, 'empty tags');
  empty.dispose();

  // Cross-module call: getStats lives in kv.stats and takes the parent
  // module's Store interface as a parameter.
  final st = wv.getStats(store);
  expect(st.totalEntries == 2, 'stats total entries == 2');
  expect(st.expiredEntries == 0, 'stats expired entries == 0');
  st.dispose();

  // Eviction listener: delete fires the isolate-local NativeCallable
  // synchronously on the calling thread.
  final evicted = <String>[];
  final sub = wv.registerEvictionListener(evicted.add);
  expect(sub > 0, 'listener id positive');
  expect(store.delete('beta'), 'delete beta');
  expect(evicted.length == 1 && evicted[0] == 'beta',
      'eviction fired for beta (got $evicted)');

  // Unregister stops delivery.
  wv.unregisterEvictionListener(sub);
  expect(store.delete('alpha'), 'delete alpha');
  expect(evicted.length == 1, 'no eviction after unregister (got $evicted)');

  // Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
  // Future settles via a NativeCallable.listener message from the producer's
  // worker thread.
  expect(store.put('doomed', payload, wv.EntryKind.volatile, 0), 'put doomed');
  final reclaimed = await store.compact();
  expect(reclaimed == 3, 'compact reclaimed 3 bytes (got $reclaimed)');
  expect(store.count() == 0, 'store empty after deletes + compact');

  store.dispose();
  print('dart/kvstore: OK');
}
