// Shared helpers for the Dart conformance consumers. run.sh copies this file
// next to each consumer's main.dart.

import 'dart:ffi';
import 'dart:io';

/// Throws when [cond] is false.
void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

/// Expects [body] to throw a [T] and returns it.
T expectThrows<T extends Object>(void Function() body, String msg) {
  try {
    body();
  } on T catch (e) {
    return e;
  }
  throw StateError('assertion failed: $msg (nothing thrown)');
}

/// Expects the future from [body] to fail with a [T] and returns it.
Future<T> expectThrowsAsync<T extends Object>(
    Future<Object?> Function() body, String msg) async {
  try {
    await body();
  } on T catch (e) {
    return e;
  }
  throw StateError('assertion failed: $msg (nothing thrown)');
}

/// The value or error [future] completes with. Attaching it right away
/// keeps an early error from counting as unhandled.
Future<Object?> outcome(Future<Object?> future) =>
    future.then<Object?>((v) => v, onError: (Object e) => e);

/// Turns the event loop until [done] holds, failing after [timeout].
Future<void> settle(bool Function() done, String msg,
    {Duration timeout = const Duration(seconds: 10)}) async {
  final watch = Stopwatch()..start();
  while (!done()) {
    if (watch.elapsed > timeout) throw StateError('timed out: $msg');
    await Future<void>.delayed(const Duration(milliseconds: 2));
  }
}

/// Checks the producer's leak counters through raw `dart:ffi`: live objects,
/// callbacks, iterators, cancel tokens, and returned allocations must all
/// drop to zero. Garbage is churned between checks so the GC runs and the
/// bindings' finalizers release whatever was left to them.
Future<void> expectNoLeaks(String prefix) async {
  final env = '${prefix.toUpperCase()}_LIBRARY';
  final lib = DynamicLibrary.open(Platform.environment[env]!);
  final live = lib.lookupFunction<Uint64 Function(Int32), int Function(int)>(
      '${prefix}_debug_live');
  List<int> counts() => [for (var k = 0; k <= 4; k++) live(k)];
  final watch = Stopwatch()..start();
  var junk = <Object>[];
  while (counts().any((c) => c != 0)) {
    if (watch.elapsed > const Duration(seconds: 20)) {
      throw StateError('leaked native resources '
          '[objects, callbacks, iterators, tokens, allocations]: ${counts()}');
    }
    for (var i = 0; i < 20000; i++) {
      junk.add(List<int>.filled(64, i));
    }
    junk = <Object>[];
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}
