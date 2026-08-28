// Conformance consumer: async-demo sample, Dart target.
//
// Drives the Future-backed async surface end to end: `runTask` settled
// through a NativeCallable.listener from the producer's worker thread and
// decoded from a value buffer into the plain TaskResult class, the typed
// InvalidNameException (extending TaskException extending WeaveFFIException)
// for an empty name, the buffered list-of-records round trip through
// `runBatch`, the direct-scalar `runNTasks`, the sync `cancelTask`, and
// `activeCallbacks` settling to zero once every task body has completed.
// Throws (non-zero exit) on any mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

Future<void> main() async {
  // Async record return: the Future resolves with a plain TaskResult.
  final result = await wv.runTask('alpha');
  expect(result.id > 0, 'runTask assigns an id');
  expect(result.value == 'completed: alpha', 'runTask value (got ${result.value})');
  expect(result.success, 'runTask success flag');

  // Typed async error: the empty name settles with InvalidNameException.
  try {
    await wv.runTask('');
    expect(false, 'expected InvalidNameException for empty name');
  } on wv.InvalidNameException catch (e) {
    expect(e.code == 1, 'InvalidName carries code 1 (got ${e.code})');
    expect(e is wv.TaskException, 'subclass of TaskException');
    expect(e is wv.WeaveFFIException, 'subclass of the brand exception');
  }

  // Buffered list-of-records both ways.
  final batch = await wv.runBatch(['a', 'b', 'c']);
  expect(
    batch.map((r) => r.value).join('|') ==
        'completed: a|completed: b|completed: c',
    'runBatch values',
  );
  expect(batch.every((r) => r.success), 'runBatch success flags');

  // Direct scalar through the async callback.
  expect(await wv.runNTasks(7) == 7, 'runNTasks echoes n');

  // Sync functions beside the async ones.
  expect(!wv.cancelTask(1), 'cancelTask reports not cancelled');

  // Every spawned task body has completed by the time its callback fires.
  expect(wv.activeCallbacks() == 0, 'activeCallbacks settles to zero');

  print('dart async-demo conformance: OK');
}
