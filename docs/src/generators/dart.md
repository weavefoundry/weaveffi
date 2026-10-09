# Dart

The Dart target generates a standalone Dart package that calls the
producer's C ABI (revision 5) through `dart:ffi`. It works in Dart
command-line apps and in Flutter apps on the desktop, Android, and iOS.
Records become value classes, rich enums sealed class hierarchies,
interfaces disposable wrapper classes, async functions `Future`s, and
callback interfaces abstract classes you implement.

Dart is a [Tier 2](../stability.md#target-tiers) target: it may lag behind
new ABI work, but it's held to the same conformance suite as every other
target.

## What gets generated

```text
dart/
  pubspec.yaml               package `{package}`, depends on `ffi`
  README.md
  lib/{package}.dart         the library: its imports and parts
  lib/src/runtime/*.dart     the runtime (loader, errors, codecs, ...) and contract tables
  lib/src/{module}.dart      one part per module (`kv.dart`, `kv/stats.dart`)
```

The whole binding is one Dart library split into parts, so you import a
single file. The package name defaults to the library's C prefix
(`kvstore` for the kvstore sample), so consumers write
`import 'package:kvstore/kvstore.dart';`. Module-level functions use their
bare names (`openStore`, `renderReport`), which global names keep unique.
Runtime sections the API doesn't use (async, callbacks, typed arrays, ...)
aren't emitted.

## Configuration

```toml
[generators.dart]
name = "kvstore"          # package name and library file (default: the C prefix)
sdk = ">=3.10.0 <4.0.0"   # the pubspec's SDK constraint (this is the default)
```

The package needs Dart 3.10 or newer, the first release with
`NativeCallable.isolateGroupBound`, which delivers void callback methods from
any thread (see [Threading](#threading)), and the `ffi` package (2.1 or
newer), which supplies `Arena` and `calloc`.

## Install, build, and load

Add the generated package as a dependency, for example with a `path:`
dependency, and run `dart pub get`:

```yaml
dependencies:
  kvstore:
    path: bindings/dart
```

The bindings open the native library on first use: `libkvstore.dylib` on
macOS, `libkvstore.so` on Linux and Android, and `kvstore.dll` on Windows,
found through the platform's normal search. Set `KVSTORE_LIBRARY` (the
identity's `{PREFIX}_LIBRARY` variable) to a full path to load a specific
build. On iOS, and on macOS when the file isn't found, the bindings look the
symbols up in the running executable, which is how a statically linked or
embedded library is found.

`weaveffi package` writes the pub package `dart/{package}/` with each desktop
platform's library under `native/<platform>/`. The packaged loader picks the
running platform's library (by `Abi.current()`) and looks for it in the
package itself, resolved with `Isolate.resolvePackageUriSync`, then under the
working directory (for a compiled app that copied `native/` next to itself),
and only then on the system search path. `{PREFIX}_LIBRARY` still wins over
all of them. See [Packaging](../guides/packaging.md).

### Load-time checks

Before any other call, the bindings check the library and throw a
`NativeLibraryException` if it can't be opened or doesn't match:

- `kvstore_abi_version()` must report revision 5.
- For every top-level module, the bindings embed the contract entries they
  were generated with (one `(id, hash, path)` record per declaration, error
  code, and callback method, each with its canonical signature as a
  comment), call `kvstore_{module}_contract`, and require each entry to be
  present with an equal hash. A missing entry fails with "kvstore:
  kv.Store.put is missing from the library"; a different hash with
  "kvstore: kv.Store.put changed since these bindings were generated".
  Entries the library has and the bindings don't are fine, so adding a
  function, an error code, or a callback method never breaks a deployed
  binding.

`NativeLibraryException` is an `Exception`, thrown from whichever call into
the library comes first, so an app can catch it and report a broken install.
The load isn't cached when it fails: the next call tries again.

## Type mapping

| IDL type | Dart type | Crosses the C ABI as |
|---|---|---|
| `i8` to `i64`, `u8` to `u64` | `int` | by value (`u64` as its two's-complement bit pattern) |
| `f32`, `f64` | `double` | by value |
| `bool` | `bool` | C `bool` |
| `string` | `String` | UTF-8 `(ptr, len)`, so interior NULs survive |
| `bytes` | `Uint8List`; a parameter takes any `List<int>` | `(ptr, len)` |
| Record | a `final class` with final fields, named constructor arguments, and value equality | value buffer |
| Plain enum | an enhanced `enum` with `value` and `fromValue` | `int32_t` |
| Rich enum | a `sealed` class with one `final` subclass per variant | value buffer |
| Interface | a `final class` wrapper | object pointer |
| `T?` of a scalar or plain enum | `T?` | a presence flag and the value (no buffer) |
| `[T]` of a number (`i8` to `i64`, `u16` to `u64`, `f32`, `f64`) | `List<int>` or `List<double>` | a C array and its element count (no buffer) |
| Any other `T?` | `T?` | value buffer (`Interface?` is a nullable pointer, `Cb?` a null vtable) |
| Any other `[T]`, `{K:V}` | `List<T>`, `Map<K, V>` | value buffer |
| `iter<T>` | `Iterable<T>` | native iterator |
| Callback interface | an `abstract class` you implement | `ctx` plus vtable |

A numeric list argument is copied into native memory for the call (a typed
list such as `Int32List` is copied as is; any other list is converted
first). A returned numeric list is a fixed-length typed list behind the
`List<int>` or `List<double>` type (`Int32List`, `Float64List`, and so on),
copied out of the producer's array before it's released. A `bytes` value
the bindings return, decode, or pass to a callback is always a `Uint8List`;
a `bytes` parameter (or a callback's `bytes` return) takes any `List<int>`,
so a list literal works too. Inside records and other value buffers,
optionals and lists keep the value-buffer encoding.

Records and rich-enum variants implement `==`, `hashCode`, and `toString`:
lists, maps, and byte arrays compare element by element, and interface
wrappers by identity. Wrapper functions and members use lowerCamelCase;
types use UpperCamelCase. A name that collides with a Dart keyword or a type
the bindings use gains a trailing `_` (`class_`, `String_`, `Int32List_`), as
does an interface member named like a member every wrapper declares or
inherits (`dispose`, `hashCode`, `toString`, `runtimeType`, `noSuchMethod`):
a method `dispose` is `dispose_()`. A record field named like one of those
`Object` members, and an error-code field named `code` or `message`, gains
the `_` too (kvstore's `CallbackFailed { message }` has a `message_`
field). See [Naming](../reference/naming.md#identifiers-in-generated-code).

Each distinct optional, list, or map type that crosses inside a value buffer
gets one private codec pair named by its canonical shape (`_pack_list_Entry`,
`_unpack_map_string_string`), built on the runtime's generic
`writeList`/`readList` helpers, so no call site inlines a loop.

## Objects and lifetime

Each wrapper holds one strong reference. Call `dispose()` when you're done;
an undisposed wrapper is released by a `NativeFinalizer` when it's
collected. Disposing twice is harmless, and using a disposed wrapper throws a
`StateError`.

```dart
final store = Store.open('/tmp/data');      // factory constructor
store.put('key', [1, 2, 3], EntryKind.persistent, null);
final copy = store.share();                 // a second reference
store.dispose();
copy.count();                               // still valid
copy.dispose();
```

Every call borrows the wrapper's pointer for its duration. If a callback
disposes a wrapper that a call in progress is using, the release waits until
that call returns, so the native object never disappears mid-call. Wrappers
are `Finalizable`, so Dart keeps them alive until the end of any call that
uses them. An object written into a value buffer (a record field, a list
element) is encoded as a fresh reference, so the wrapper stays usable.

## Errors

Each error domain becomes an exception class extending `NativeException`,
named by the shared rule (`KvError` is `KvException`, `KitchenErrors` is
`KitchenException`), with one final subclass per code (`KeyNotFound` is
`KeyNotFoundException`). Payload fields are typed properties, and the
code's documented message is the default for the optional trailing
`message` argument:

```dart
try {
  store.get('missing');
} on KeyNotFoundException catch (e) {
  print('${e.code}: ${e.message} (${e.key})');   // 1001: key not found: missing (missing)
} on KvException catch (e) {
  // any other KvError code, including ones these bindings don't know
}
```

A module may declare several domains, and a function names the one it
throws; codes are only unique within a domain, so the bindings decode a
failure by the function's domain (calculator's `CalcError.DivisionByZero`
and `ParseError.NotANumber` are both code 1).

Domains are open. A positive code the bindings don't declare (from a newer
library that added a code) arrives as the domain class itself
(`KvException`) with its code and message, never as a crash, so catch the
domain class to stay forward compatible. A negative runtime code becomes a
plain `NativeException` with that code. The runtime codes are constants on
`NativeException`: `genericCode` (-1), `panicCode` (-2), `marshalCode`
(-3), `foreignCode` (-4, a callback implementation failed), and
`cancelledCode` (-5, raised as `CancelledException`).

A function declared `throws: any` fails with a plain `NativeException`
carrying `genericCode` and the producer's message (kvstore's `importLines`
fails with "line 2: expected key=value").

A failure of a function that isn't declared to throw is a bug in the
producer, so it follows the [trap policy](../guides/errors-and-memory.md#the-trap-policy):
the binding throws a `NativeError`, an `Error` (not an `Exception`) carrying
the runtime `code` and the producer's `message`. A marshalling failure of
such a function (codec's `echoChar('ab')`) is a `NativeError` with
`marshalCode`. Cancellation always surfaces as `CancelledException`.

## Async and cancellation

An async function returns a `Future`. The producer completes it from a
worker thread through a `NativeCallable.listener`, and the result is
decoded on the event loop.

A cancellable function takes an optional named `CancelToken`:

```dart
final token = CancelToken();
final pending = store.compact(60000, cancelToken: token);
token.cancel();
try {
  await pending;
} on CancelledException {
  // completed with code -5 without waiting for the work
}
```

One token can serve any number of calls, a token that's already cancelled
cancels a call at launch, and cancelling after completion does nothing. The
bindings create a native token for each call, cancel it when the
`CancelToken` is cancelled, and destroy it when the call completes.

## Callbacks

Implement the generated abstract class and pass an instance. The bindings
keep it in a table until the producer releases it through the vtable's
`free`, so you don't need to hold another reference. An optional callback
parameter (`Policy?`) takes `null` for none, which passes a null vtable.

```dart
class AdmitAll implements Policy {
  @override
  int? ttlFor(String key, int? requested) => requested;

  @override
  Entry admit(Entry entry) {
    if (entry.key.startsWith('secret')) {
      throw RejectedException(entry.key, 'no secrets');
    }
    return entry;
  }

  @override
  Store route(String key, Store home) => home;
}

store.setPolicy(AdmitAll());
```

A method that returns a value hands it to the producer in the shape the ABI
expects: a scalar by value; an optional scalar as a presence flag and an out
value; an object as a fresh strong reference the producer adopts; and a
string, bytes, numeric list, or record as a run the bindings allocate with
`{prefix}_alloc` and the producer frees. Arguments arrive copied (a numeric
list as a typed list), and object arguments are owned by the
implementation; call `dispose()` on them when you're done (or let the
finalizer release them). A method may call back into the library: every
call has its own error and out slots, so a call made from inside a callback
never disturbs the call that's waiting on it.

Exceptions never unwind into the producer. In a method declared to throw a
domain, throwing that domain's exception (here a `KvException` subclass)
reports its code and its fields, encoded as a value buffer, so the producer
sees exactly that error: `put` above fails with the same
`RejectedException` (with the message the producer renders from its
fields). Any other exception, and any exception from a `throws: any`
method, reports `genericCode` (-1) with the exception's text (a
`NativeException`'s `message`); from a method that isn't declared to throw,
it reports `foreignCode` (-4).

### Threading

How a method runs depends on its return type:

- **A method that returns a value** is a `NativeCallable.isolateLocal`
  trampoline. It runs synchronously, so it can only run on the isolate's
  thread while a call from Dart is in progress, as a policy or a filter
  consulted during the call is.
- **A void method** is a `NativeCallable.isolateGroupBound` forwarder. The
  producer may call it from any thread; the forwarder copies its arguments
  and returns at once, and the method runs later on the event loop, in the
  zone that passed the instance. An exception there is an uncaught error of
  that zone, and a void method can't fail the producer's call even when it's
  declared to throw. The vtable's `free` is forwarded the same way, after
  every call made before it.

Every vtable the bindings pass is flagged thread-affine
(`{PREFIX}_VTABLE_THREAD_AFFINE`, ABI section 1.7). The producer records the
thread that passed it and refuses to call a value-returning method from any
other thread: the method doesn't run, and the producer sees it fail with
`foreignCode` (-4) and the message "callback called off its thread". Without
the flag, the Dart VM would abort the process, since only an `isolateLocal`
callable can run the isolate's code synchronously and the VM aborts before
any Dart code runs when one is called from a thread that isn't running the
isolate. In the kvstore sample, `compact` tells listeners from a worker
thread: a Dart `Listener`'s `accepts` is refused there, so the listener is
detached, and compaction carries on.

## Iterators

An `iter<T>` return is a lazy `Iterable<T>`: each step pulls one element from
the native iterator, and iterating again starts a new one. The native
iterator is destroyed when iteration finishes or fails, or by a finalizer
when an iteration is abandoned (`first`, `take`, a `break`) and collected.

```dart
for (final key in store.keys('user.')) {
  print(key);
}
```

## Performance notes

Every call takes a frame from a per-isolate pool: the native error slot, the
return's out slots, and an `Arena` for staged arguments. A call made from a
callback takes a frame of its own, and a frame goes back to the pool when
its call returns, so steady-state calls allocate no native memory for their
slots. Optional scalars and numeric lists skip the value buffer entirely.
When the API declares no callback interfaces, synchronous calls are bound as
leaf calls (`isLeaf: true`), which skips the VM's safepoint transition; a
leaf call blocks this isolate group's garbage collector until it returns, so
it suits fast functions.

## Known limitations

- A value-returning callback method can only run on the isolate's thread,
  during a call from Dart. A producer that calls one from its own threads
  (a worker thread, an async executor) gets a -4 failure instead of a
  result; void methods are safe from any thread (see
  [Threading](#threading)).
- The producer pins a vtable to the OS thread that passed it. The main
  isolate of a Dart program, and Flutter's root isolate, stay on one
  thread, but the VM may move another isolate (one started with
  `Isolate.spawn` or `Isolate.run`) to a different thread between event-loop
  turns. In such an isolate, pass an implementation and use it within one
  synchronous stretch of code, or expect its value-returning methods to fail
  with -4 after an `await`.
- Void callback methods are always delivered asynchronously, even when the
  producer calls them on the isolate's thread, so their exceptions can't fail
  the producer's call.
- `NativeCallable.isolateGroupBound`, which callback interfaces rely on, is
  marked experimental in the Dart SDK ("may change in the future"), so a
  future SDK release could change how it's enabled.
- Library loading uses `DynamicLibrary`; Flutter's native-assets build hooks
  aren't generated.
