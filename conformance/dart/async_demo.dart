// Conformance consumer: async-demo sample, Dart target.
//
// Futures completed from the producer's worker threads: a record result, the
// typed InvalidNameException, a buffered list both ways, a direct scalar,
// many concurrent calls, and cancellation through CancelToken (cancelling a
// pending call, a token cancelled before launch, one token shared by two
// calls, and a cancel after completion), each cancelled call failing with
// CancelledException (code -5) long before its timeout.

import 'package:async_demo/async_demo.dart' as tasks;

import 'support.dart';

Future<void> run() async {
  final result = await tasks.runTask('alpha');
  expect(result.id > 0, 'runTask id');
  expect(result.value == 'completed: alpha', 'runTask value (${result.value})');
  expect(result.success, 'runTask success');

  final invalid = await expectThrowsAsync<tasks.InvalidNameException>(
      () => tasks.runTask(''), 'empty name');
  expect(invalid.code == 1 && invalid is tasks.TaskException, 'InvalidName');

  final batch = await tasks.runBatch(<String>['a', 'b', 'c']);
  expect(batch.map((r) => r.value).join('|') ==
          'completed: a|completed: b|completed: c',
      'runBatch values');
  expect((await tasks.runBatch(<String>[])).isEmpty, 'runBatch empty');

  expect(await tasks.runNTasks(7) == 7, 'runNTasks');
  final many = await Future.wait(
      <Future<int>>[for (var i = 0; i < 64; i++) tasks.runNTasks(i)]);
  expect(many.length == 64 && many[63] == 63, 'concurrent runNTasks');

  // A timeout that elapses completes normally, with or without a token.
  expect(await tasks.wait(20) == 20, 'wait elapses');
  final idle = tasks.CancelToken();
  expect(await tasks.wait(10, cancelToken: idle) == 10, 'uncancelled token');
  idle.cancel();
  expect(idle.isCancelled, 'cancel after completion is harmless');

  // Cancelling a pending call completes it with CancelledException.
  final watch = Stopwatch()..start();
  final token = tasks.CancelToken();
  final pending = outcome(tasks.wait(60000, cancelToken: token));
  await Future<void>.delayed(const Duration(milliseconds: 20));
  token.cancel();
  final cancelled = await pending;
  expect(cancelled is tasks.CancelledException, 'cancelled wait (got $cancelled)');
  final code = (cancelled! as tasks.NativeException).code;
  expect(code == -5 && code == tasks.NativeException.cancelledCode,
      'cancelled code (got $code)');
  expect(watch.elapsed < const Duration(seconds: 5), 'cancel is prompt');

  // A token cancelled before launch, and one token shared by two calls.
  await expectThrowsAsync<tasks.CancelledException>(
      () => tasks.wait(60000, cancelToken: token), 'pre-cancelled token');
  final shared = tasks.CancelToken();
  final both = Future.wait(<Future<Object?>>[
    outcome(tasks.wait(60000, cancelToken: shared)),
    outcome(tasks.wait(60000, cancelToken: shared)),
  ]);
  shared.cancel();
  shared.cancel();
  final outcomes = await both;
  expect(outcomes.every((o) => o is tasks.CancelledException),
      'one token cancels both calls (got $outcomes)');
  expect(watch.elapsed < const Duration(seconds: 10), 'all cancels prompt');

  await settle(() => tasks.activeCallbacks() == 0, 'task bodies finish');
}

Future<void> main() async {
  await run();
  await expectNoLeaks('async_demo');
  print('dart/async-demo: OK');
}
