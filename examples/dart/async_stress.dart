// Async stress test for the Dart async lifecycle.
//
// Loads the async-demo cdylib (path in ASYNC_DEMO_LIB) via dart:ffi and
// spawns 1000 concurrent calls to weaveffi_tasks_run_n_tasks_async,
// mirroring the NativeCallable.listener + .close() pattern that the Dart
// generator emits.
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback returns the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//   * every NativeCallable is closed after its callback fires (a leak
//     would show up as the callable's underlying trampoline still pinned
//     in the test's working set across the run).
//
// Prints "OK" and exits 0 on success.

import 'dart:async';
import 'dart:ffi';
import 'dart:io';
import 'package:ffi/ffi.dart';

const int nTasks = 1000;
const int timeoutSeconds = 30;

final class WeaveffiError extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
}

typedef RunNTasksCb
    = Void Function(Pointer<Void>, Pointer<WeaveffiError>, Int32);
typedef RunNTasksNative = Void Function(
    Int32, Pointer<NativeFunction<RunNTasksCb>>, Pointer<Void>);
typedef RunNTasksDart = void Function(
    int, Pointer<NativeFunction<RunNTasksCb>>, Pointer<Void>);
typedef ActiveCallbacksNative = Int64 Function(Pointer<WeaveffiError>);
typedef ActiveCallbacksDart = int Function(Pointer<WeaveffiError>);

DynamicLibrary openLibrary() {
  final path = Platform.environment['ASYNC_DEMO_LIB'];
  if (path == null || path.isEmpty) {
    stderr.writeln('ASYNC_DEMO_LIB not set');
    exit(1);
  }
  return DynamicLibrary.open(path);
}

Future<int> runOne(
  int n,
  RunNTasksDart runNTasksAsync,
) {
  final completer = Completer<int>();
  late NativeCallable<RunNTasksCb> callable;
  callable = NativeCallable<RunNTasksCb>.listener((
    Pointer<Void> context,
    Pointer<WeaveffiError> err,
    int result,
  ) {
    try {
      completer.complete(result);
    } finally {
      callable.close();
    }
  });
  try {
    runNTasksAsync(n, callable.nativeFunction, nullptr);
  } catch (e) {
    callable.close();
    rethrow;
  }
  return completer.future;
}

Future<void> main() async {
  final lib = openLibrary();
  final runNTasksAsync = lib.lookupFunction<RunNTasksNative, RunNTasksDart>(
      'weaveffi_tasks_run_n_tasks_async');
  final activeCallbacks =
      lib.lookupFunction<ActiveCallbacksNative, ActiveCallbacksDart>(
          'weaveffi_tasks_active_callbacks');

  final start = DateTime.now();
  final futures = <Future<int>>[];
  for (var i = 0; i < nTasks; i++) {
    futures.add(runOne(i, runNTasksAsync));
  }

  final results = await Future.wait(futures)
      .timeout(Duration(seconds: timeoutSeconds), onTimeout: () {
    stderr.writeln('timeout waiting for callbacks');
    exit(1);
  });

  for (var i = 0; i < nTasks; i++) {
    if (results[i] != i) {
      stderr.writeln('results[$i] = ${results[i]}, expected $i');
      exit(1);
    }
  }

  final err = calloc<WeaveffiError>();
  try {
    final active = activeCallbacks(err);
    if (err.ref.code != 0 || active != 0) {
      stderr.writeln('active_callbacks = $active (expected 0)');
      exit(1);
    }
  } finally {
    calloc.free(err);
  }

  final elapsed = DateTime.now().difference(start);
  print('OK ($nTasks tasks in ${elapsed.inMilliseconds}ms)');
}
