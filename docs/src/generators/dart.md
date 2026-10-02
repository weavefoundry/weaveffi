# Dart

The Dart target generates a standalone Dart package that calls the C ABI
through `dart:ffi`. It works in Dart command-line apps and in Flutter apps on
the desktop, Android, and iOS.

## What gets generated

```text
dart/
  pubspec.yaml        package `{prefix}`, depends on `ffi`
  README.md
  lib/{prefix}.dart   the whole binding: runtime, types, and functions
```

The package name is the library's C prefix (`kvstore` for the kvstore
sample), so consumers write `import 'package:kvstore/kvstore.dart';`. Set
`package_name` under `[generators.dart]` to choose another name; the library
file follows it. With `strip_module_prefix = false`, module-level functions
keep their module path (`kvStatsGetStats` instead of `getStats`).

## Install, build, and load

Add the generated package as a dependency, for example with a `path:`
dependency, and run `dart pub get`:

```yaml
dependencies:
  kvstore:
    path: generated/dart
```

The bindings open the native library on first use: `libkvstore.dylib` on
macOS, `libkvstore.so` on Linux and Android, and `kvstore.dll` on Windows,
found through the platform's normal search. Set `KVSTORE_LIBRARY` (the
identity's `{PREFIX}_LIBRARY` variable) to a full path to load a specific
build. On iOS, and on macOS when the file isn't found, the bindings look the
symbols up in the running executable, which is how a statically linked or
embedded library is found. `weaveffi package` bundles prebuilt desktop
libraries under `native/<platform>/` and tries those first.

Before any other call, the bindings check the library: `kvstore_abi_version()`
must report 3, and every top-level module's checksum function must match the
value the bindings were generated against. A mismatch throws a `StateError`
that names the module and asks you to regenerate the bindings.

The package needs Dart 3.12 or newer and the `ffi` package. Flutter native
assets (build hooks that compile the library) aren't generated; ship the
library with your app as you would any other native dependency.

## Type mapping

| IDL type | Dart type | Crosses the C ABI as |
|---|---|---|
| `i8` to `i64`, `u8` to `u64` | `int` | by value (`u64` as its two's-complement bit pattern) |
| `f32`, `f64` | `double` | by value |
| `bool` | `bool` | C `bool` |
| `string` | `String` | UTF-8 `(ptr, len)`, so interior NULs survive |
| `bytes` | `List<int>` (a `Uint8List` when returned) | `(ptr, len)` |
| Record | a class with final fields and named constructor arguments | value buffer |
| Plain enum | an enhanced `enum` with `value` and `fromValue` | `int32_t` |
| Rich enum | a `sealed` class with one subclass per variant | value buffer |
| Interface | a `final class` wrapper | object pointer |
| `T?` | `T?` | value buffer (`Interface?` is a nullable pointer) |
| `[T]`, `{K:V}` | `List<T>`, `Map<K, V>` | value buffer |
| `iter<T>` | `Iterable<T>` | native iterator |
| Callback interface | an `abstract class` you implement | `ctx` plus vtable |

Wrapper functions and members use lowerCamelCase; types use UpperCamelCase.
A name that collides with a Dart keyword or a type the bindings use gains a
trailing `_` (`class_`, `String_`).

## Objects and lifetime

Each wrapper holds one strong reference. Call `dispose()` when you're done;
an undisposed wrapper is released by a `NativeFinalizer` when it's
collected. Disposing twice is harmless, and using a disposed wrapper throws a
`StateError`.

```dart
final store = kv.Store.open('/tmp/data');   // factory constructor
store.put('key', [1, 2, 3], kv.EntryKind.persistent, null);
final copy = store.share();                  // a second reference
store.dispose();
copy.count();                                // still valid
copy.dispose();
```

Every call borrows the wrapper's pointer for its duration. If a callback
disposes a wrapper that a call in progress is using, the release waits until
that call returns, so the native object never disappears mid-call. Wrappers
are `Finalizable`, so Dart keeps them alive until the end of any call that
uses them. An object written into a value buffer (a record field, a list
element) is encoded as a fresh reference, so the wrapper stays usable.

## Errors

Every failure is a `NativeException` with a `code` and a `message`. A
module's error domain becomes a subclass, with one subclass per code:

```dart
try {
  store.get('missing');
} on kv.KeyNotFoundException catch (e) {
  print('${e.code}: ${e.message}');   // 1001: key not found
} on kv.KvException {
  // any other KvError code
}
```

Only functions declared `throws` raise domain exceptions. The runtime codes
are constants on `NativeException`: `genericCode` (-1), `panicCode` (-2),
`marshalCode` (-3), `foreignCode` (-4, a callback implementation threw), and
`cancelledCode` (-5, raised as `CancelledException`). A domain code with
fields carries them as typed properties on its exception.

## Async and cancellation

An async function returns a `Future`. The producer completes it from a
worker thread through a `NativeCallable.listener`, and the result is
decoded on the event loop.

A cancellable function takes an optional named `CancelToken`:

```dart
final token = kv.CancelToken();
final pending = store.compact(cancelToken: token);
token.cancel();
try {
  await pending;
} on kv.CancelledException {
  // completed with code -5 without waiting for the work
}
```

One token can serve any number of calls, a token that's already cancelled
cancels a call at launch, and cancelling after completion does nothing. The
bindings create a native token for each call, cancel it when the
`CancelToken` is cancelled, and destroy it when the call completes.

## Callbacks

Implement the generated abstract class and pass an instance. The bindings
keep it in a table until the producer releases it, so you don't need to hold
another reference.

```dart
class Logger extends kv.EvictionListener {
  @override
  bool onEvict(kv.Entry entry, kv.EvictionReason reason) {
    print('evicted ${entry.key}');
    return true;
  }
}

store.setEvictionListener(Logger());
```

How a method runs depends on its return type:

- A method that returns a value is a `NativeCallable.isolateLocal`
  trampoline. It runs synchronously, so the producer must call it on the
  isolate's thread while a call from Dart is in progress. If it throws, the
  producer's call fails with `foreignCode` (-4) and the exception's text.
- A void method is a `NativeCallable.isolateGroupBound` forwarder. The
  producer may call it from any thread; the forwarder copies its arguments
  and returns at once, and the method runs later on the event loop, in the
  zone that passed the instance. An exception there is an uncaught error of
  that zone.

Object arguments are owned by the implementation; call `dispose()` on them
when you're done.

## Iterators

An `iter<T>` return is a lazy `Iterable<T>`: each step pulls one element from
the native iterator, and iterating again starts a new one. The native
iterator is destroyed when iteration finishes or fails, or by a finalizer
when an iteration is abandoned (`first`, `take`, a `break`) and collected.

```dart
for (final key in store.listKeys('user:')) {
  print(key);
}
```

## Performance notes

Synchronous calls share one error slot and one length slot per isolate, and
stage arguments in a single `Arena` per call. When the API declares no
callback interfaces, synchronous calls are bound as leaf calls (`isLeaf:
true`), which skips the VM's safepoint transition; a leaf call blocks this
isolate group's garbage collector until it returns, so it suits fast
functions.

## Known limitations

- A value-returning callback method invoked from a thread other than the
  isolate's aborts the process. That's a Dart VM rule for
  `NativeCallable.isolateLocal`, and pure Dart can't detect the thread first:
  an any-thread entry point can't re-enter the isolate to run the method.
  Producers must call value-returning methods synchronously during a call
  from Dart.
- Void callback methods are always delivered asynchronously, even when the
  producer calls them on the isolate's thread, so their exceptions can't fail
  the producer's call.
- `NativeCallable.isolateGroupBound` is marked experimental in the Dart SDK.
- Library loading uses `DynamicLibrary`; Flutter's native-assets build hooks
  aren't generated.
