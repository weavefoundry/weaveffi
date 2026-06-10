// Conformance consumer: kvstore sample, Dart target.
//
// Exercises the complex-return marshaling the Dart backend previously stubbed:
// the `List<int>` bytes getter (`Entry.value`), the `List<String>` list getter
// (`Entry.tags`), the `Map<String, String>` getter over the triple-pointer ABI
// (`Entry.metadata`), the nullable-scalar getter (`Entry.expiresAt`), the
// iterator-backed `listKeys`, and the fluent builder's bytes/optional/list/map
// *input* marshaling (`build()` -> the C `create` symbol). Also covers the
// `kv.stats` submodule, the NativeCallable.isolateLocal eviction listener
// (register -> fire synchronously on delete -> unregister), and the
// Future-returning `compactAsync` settled through a NativeCallable.listener
// from the producer's worker thread. Throws (non-zero exit) on any mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

Future<void> main() async {
  final store = wv.openStore('/tmp/conformance-kvstore-dart');

  final payload = <int>[1, 2, 3];
  expect(wv.put(store, 'alpha', payload, wv.EntryKind.persistent, null),
      'put alpha');
  expect(wv.put(store, 'beta', payload, wv.EntryKind.volatile, null),
      'put beta');

  expect(wv.count(store) == 2, 'count == 2');

  // Iterator-backed list-of-string function return.
  final keys = wv.listKeys(store, null).toList()..sort();
  expect(keys.length == 2 && keys[0] == 'alpha' && keys[1] == 'beta',
      'list_keys values');

  // Builder input marshaling: scalars, bytes, optional, list, and map.
  final entry = wv.EntryBuilder()
      .withId(7)
      .withKey('alpha')
      .withValue(payload)
      .withCreatedAt(1000)
      .withExpiresAt(null)
      .withTags(<String>['hot', 'fast'])
      .withMetadata(<String, String>{'source': 'test', 'env': 'prod'})
      .build();
  expect(entry.id == 7, 'entry id == 7');

  // Nullable-scalar getter: expires_at was left unset.
  expect(entry.expiresAt == null, 'entry expiresAt null');

  // List<int> bytes getter.
  final value = entry.value;
  expect(value.length == 3 && value[0] == 1 && value[2] == 3, 'entry value');

  // List<String> list getter.
  final tags = entry.tags..sort();
  expect(tags.length == 2 && tags[0] == 'fast' && tags[1] == 'hot',
      'entry tags');

  // Map<String, String> getter over the triple-pointer out-params.
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

  // kv.stats submodule.
  final st = wv.getStats(store);
  expect(st.totalEntries == 2, 'stats total entries == 2');
  st.dispose();

  // Eviction listener: delete fires the isolate-local NativeCallable
  // synchronously on the calling thread.
  final evicted = <String>[];
  final sub = wv.registerEvictionListener(evicted.add);
  expect(sub > 0, 'listener id positive');
  expect(wv.delete(store, 'beta'), 'delete beta');
  expect(evicted.length == 1 && evicted[0] == 'beta',
      'eviction fired for beta (got $evicted)');

  // Unregister stops delivery.
  wv.unregisterEvictionListener(sub);
  expect(wv.delete(store, 'alpha'), 'delete alpha');
  expect(evicted.length == 1, 'no eviction after unregister (got $evicted)');

  // Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
  // Future settles via a NativeCallable.listener message from the producer's
  // worker thread.
  expect(wv.put(store, 'doomed', payload, wv.EntryKind.volatile, 0), 'put doomed');
  final reclaimed = await wv.compactAsync(store);
  expect(reclaimed == 3, 'compact reclaimed 3 bytes (got $reclaimed)');
  expect(wv.count(store) == 0, 'store empty after deletes + compact');

  store.dispose();
  print('dart/kvstore: OK');
}
