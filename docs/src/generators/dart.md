# Dart

## Overview

The Dart target produces a pure-Dart FFI package that wraps the C ABI
using [`dart:ffi`](https://dart.dev/interop/c-interop). It opens the
shared library with `DynamicLibrary.open` and resolves each symbol via
`lookupFunction`. There's no native compilation step or `ffigen` run
required; the generated `.dart` file is ready to import.

## What gets generated

| File | Purpose |
|------|---------|
| `dart/lib/weaveffi.dart` | `dart:ffi` bindings: loader, typedefs, lookups, wrappers, struct/enum classes |
| `dart/pubspec.yaml` | Package metadata and `package:ffi` dependency |
| `dart/README.md` | Basic usage instructions |

## Type mapping

| IDL type     | Dart type           | Native FFI type        | Dart FFI type        |
|--------------|---------------------|------------------------|----------------------|
| `i32`        | `int`               | `Int32`                | `int`                |
| `u32`        | `int`               | `Uint32`               | `int`                |
| `i64`        | `int`               | `Int64`                | `int`                |
| `f64`        | `double`            | `Double`               | `double`             |
| `i8`         | `int`               | `Int8`                 | `int`                |
| `i16`        | `int`               | `Int16`                | `int`                |
| `u8`         | `int`               | `Uint8`                | `int`                |
| `u16`        | `int`               | `Uint16`               | `int`                |
| `u64`        | `int`               | `Uint64`               | `int`                |
| `f32`        | `double`            | `Float`                | `double`             |
| `bool`       | `bool`              | `Int32`                | `int`                |
| `string`     | `String`            | `Pointer<Utf8>`        | `Pointer<Utf8>`      |
| `bytes`      | `List<int>`         | `Pointer<Uint8>`       | `Pointer<Uint8>`     |
| `handle`     | `int`               | `Int64`                | `int`                |
| `StructName` | `StructName`        | `Pointer<Void>`        | `Pointer<Void>`      |
| `InterfaceName` | `InterfaceName`  | `Pointer<Void>`        | `Pointer<Void>`      |
| `EnumName` (plain) | `EnumName`    | `Int32`                | `int`                |
| `EnumName` (rich)  | `EnumName`    | `Pointer<Void>`        | `Pointer<Void>`      |
| `T?`         | `T?`                | same as inner type     | same as inner type   |
| `[T]`        | `List<T>`           | `Pointer<Void>`        | `Pointer<Void>`      |
| `{K: V}`     | `Map<K, V>`         | `Pointer<Void>`        | `Pointer<Void>`      |
| `iter<T>`    | `Iterable<T>` (lazy) | `Pointer<Void>`       | `Pointer<Void>`      |

Booleans cross as `Int32` (`0`/`1`) and the wrapper converts both ways.

## Example IDL → generated code

```yaml
version: "0.5.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        doc: Type of contact
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        doc: A contact record
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"
```

The loader auto-detects the platform:

```dart
DynamicLibrary _openLibrary() {
  // An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
  // specific build artifact regardless of its file name or location.
  final override = Platform.environment['WEAVEFFI_LIBRARY'];
  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);
  if (Platform.isMacOS) return DynamicLibrary.open('libweaveffi.dylib');
  if (Platform.isLinux) return DynamicLibrary.open('libweaveffi.so');
  if (Platform.isWindows) return DynamicLibrary.open('weaveffi.dll');
  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');
}

final DynamicLibrary _lib = _openLibrary();
```

Enums become Dart enhanced enums:

```dart
/// Type of contact
enum ContactType {
  personal(0),
  work(1),
  other(2),
  ;
  const ContactType(this.value);
  final int value;
  static ContactType fromValue(int value) =>
      ContactType.values.firstWhere((e) => e.value == value);
}
```

Structs are wrapped in classes with a `dispose()` method and getter
methods that call the C accessors:

```dart
/// A contact record
class Contact {
  final Pointer<Void> _handle;
  Contact._(this._handle);

  void dispose() {
    _weaveffiContactsContactDestroy(_handle);
  }

  String get name {
    final result = _weaveffiContactsContactGetName(_handle);
    final value = result.toDartString();
    _weaveffiFreeString(result);
    return value;
  }
}
```

String getters copy the returned pointer with `toDartString()` and
release the producer's allocation with `weaveffi_free_string`.

Each function emits a native typedef, Dart typedef, lookup, and
top-level wrapper:

```dart
typedef _NativeWeaveffiContactsCreateContact =
    Int64 Function(Pointer<Utf8>, Pointer<Utf8>, Int32, Pointer<_WeaveFFIError>);
typedef _DartWeaveffiContactsCreateContact =
    int Function(Pointer<Utf8>, Pointer<Utf8>, int, Pointer<_WeaveFFIError>);
final _weaveffiContactsCreateContact = _lib.lookupFunction<
    _NativeWeaveffiContactsCreateContact,
    _DartWeaveffiContactsCreateContact>('weaveffi_contacts_create_contact');

int createContact(String name, String? email, ContactType contactType) {
  final err = calloc<_WeaveFFIError>();
  final namePtr = name.toNativeUtf8();
  try {
    final result = _weaveffiContactsCreateContact(
        namePtr, email, contactType.value, err);
    _checkError(err);
    return result;
  } finally {
    calloc.free(namePtr);
    calloc.free(err);
  }
}
```

Wrapper names are lowerCamelCase with the IDL module prefix stripped
by default (a `kv.open_store` function would surface as `openStore`,
not `kvOpenStore`); the C symbols keep their full names. Set
`strip_module_prefix: false` in the Dart generator config (or under
`[global]`) to keep module-prefixed wrapper names.

## Typed errors

The package defines `WeaveFFIException` with `code` and `message`
fields. A module's error domain adds an exception subclass named by
replacing the trailing `Error` stem with `Exception` (`KvError`
becomes `KvException`) plus one subclass per code, and a mapper that
falls back to `WeaveFFIException` for codes outside the domain. From
the `kvstore` sample:

```dart
/// Typed error domain `KvError` declared by module `kv`.
class KvException extends WeaveFFIException {
  KvException(super.code, super.message);
}

/// key not found
class KeyNotFoundException extends KvException {
  KeyNotFoundException([String message = 'key not found']) : super(1001, message);
}

// ExpiredException, StoreFullException, IoException follow the same shape.

WeaveFFIException _mapKvException(int code, String message) {
  switch (code) {
    case 1001:
      return KeyNotFoundException(message);
    // ... 1002, 1003, 1004 ...
    default:
      return WeaveFFIException(code, message);
  }
}
```

Only callables marked `throws: true` in the IDL check their error slot
with `_checkKvException` (their doc comments read
`Throws [KvException] on domain errors.`); catching
`KeyNotFoundException` or `KvException` works as usual. A callable
without `throws` uses the generic `_checkError`, which throws
`WeaveFFIException` only if the producer misbehaves.

## Interfaces

An `interfaces:` entry becomes a class holding the opaque pointer. A
constructor named `new` renders as an unnamed `factory` (so
`ContactBook()` just works); other constructors become named factories
(`Store.open(path)`). Methods are lowerCamelCase instance methods,
statics are static methods, and `dispose()` releases the native
object. From the `kvstore` sample (trimmed):

```dart
/// An embedded key-value store owning its entries
class Store {
  final Pointer<Void> _handle;
  Store._(this._handle);

  /// Releases the native object reference.
  void dispose() {
    _weaveffiKvStoreDestroy(_handle);
  }

  /// Open (or create) a store backed by the given filesystem path
  ///
  /// Throws [KvException] on domain errors.
  factory Store.open(String path) {
    final pathPtr = path.toNativeUtf8();
    final err = calloc<_WeaveFFIError>();
    try {
      final result = _weaveffiKvStoreOpen(pathPtr, err);
      _checkKvException(err);
      return Store._(result);
    } finally {
      calloc.free(pathPtr);
      calloc.free(err);
    }
  }

  bool put(String key, List<int> value, EntryKind kind, int? ttlSeconds) { /* throws KvException */ }
  Entry? get(String key) { /* throws KvException */ }
  Iterable<String> listKeys(String? prefix) sync* { /* see Iterators */ }
  int count() { /* generic check only (no throws) */ }

  /// Throws [KvException] on domain errors.
  Future<int> compact() { /* see Async support */ }

  @Deprecated('use put() with explicit kind')
  bool legacyPut(String key, List<int> value) { /* ... */ }

  /// The largest number of live entries one store will hold
  static int defaultCapacity() { /* ... */ }
}
```

Functions elsewhere in the IDL pass the wrapper's handle across the
boundary (`getStats(store)` returns a new `Stats`). There's no
finalizer; call `dispose()` when done, ideally in `try`/`finally`:

```dart
final store = Store.open('/tmp/cache.kv');
try {
  store.put('alpha', [1], EntryKind.persistent, null);
  print('${store.count()} / ${Store.defaultCapacity()}');
  final reclaimed = await store.compact();
} finally {
  store.dispose();
}
```

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style enum surfaces as a Dart `enum` and crosses as an
`Int32`; a rich enum instead lowers to an **opaque object handle**, so
the generator emits a wrapper class with the same ownership model as a
struct wrapper, a `Pointer<Void>` freed by an explicit `dispose()`.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`, the generator emits a companion `ShapeTag` enum, one
`factory` per variant, a `tag` getter that maps the discriminant back to
`ShapeTag`, and a getter per payload field:

```dart
/// An algebraic shape (sum type with associated data)
enum ShapeTag {
  empty(0),
  circle(1),
  rectangle(2),
  labeled(3),
  ;
  const ShapeTag(this.value);
  final int value;

  static ShapeTag fromValue(int value) =>
      ShapeTag.values.firstWhere((e) => e.value == value);
}

/// An algebraic shape (sum type with associated data)
class Shape {
  final Pointer<Void> _handle;
  Shape._(this._handle);

  void dispose() {
    _weaveffiShapesShapeDestroy(_handle);
  }

  ShapeTag get tag =>
      ShapeTag.fromValue(_weaveffiShapesShapeTag(_handle));

  /// A circle with a radius
  factory Shape.circle(double radius) {
    final err = calloc<_WeaveFFIError>();
    try {
      final result = _weaveffiShapesShapeCircleNew(radius, err);
      _checkError(err);
      return Shape._(result);
    } finally {
      calloc.free(err);
    }
  }

  /// Radius in points
  double get circleRadius {
    final result = _weaveffiShapesShapeCircleGetRadius(_handle);
    return result;
  }

  int get labeledCount {
    final result = _weaveffiShapesShapeLabeledGetCount(_handle);
    return result;
  }
}
```

The rest of the surface follows the same shape: factories
`Shape.empty()`, `Shape.circle(radius)`, `Shape.rectangle(width,
height)`, and `Shape.labeled(label, count)`; getters `circleRadius`,
`rectangleWidth`, `rectangleHeight`, `labeledLabel`, and `labeledCount`.
Each resolves a `weaveffi_shapes_Shape_<Variant>_new` /
`weaveffi_shapes_Shape_<Variant>_get_<field>` symbol, and
`weaveffi_shapes_Shape_tag` backs the `tag` getter.

Construct a couple of variants, read the tag and a field, then pass the
wrapper to a top-level function:

```dart
final circle = Shape.circle(2.0);
final labeled = Shape.labeled('unit', 3);
try {
  if (circle.tag == ShapeTag.circle) {
    print(circle.circleRadius);        // 2.0
  }
  print(labeled.labeledCount);         // 3

  print(describe(circle));             // render via the C ABI
  final bigger = scale(circle, 3.0);   // returns a new Shape
  bigger.dispose();
} finally {
  circle.dispose();
  labeled.dispose();
}
```

**Ownership:** a `Shape` wraps a `Pointer<Void>` that you own; call
`dispose()` (which invokes `weaveffi_shapes_Shape_destroy`) exactly as
with struct wrappers. The `Shape` returned by `scale` is a separate
handle you also dispose.

## Build instructions

Standalone Dart:

1. Generate the bindings:

   ```bash
   weaveffi generate api.yaml -o generated --target dart
   ```

2. Build the Rust shared library:

   ```bash
   cargo build --release -p your_library
   ```

3. Make the cdylib findable at runtime:

   - macOS: `DYLD_LIBRARY_PATH=$PWD/../../target/release dart run example/main.dart`
   - Linux: `LD_LIBRARY_PATH=$PWD/../../target/release dart run example/main.dart`
   - Windows: place `weaveffi.dll` next to the script or add its
     directory to `PATH`.

Flutter:

1. Generate the bindings as above.
2. Cross-compile the Rust cdylib for every Flutter target you support
   (`aarch64-apple-ios`, `aarch64-linux-android`, `x86_64-apple-darwin`,
   etc.).
3. Reference the generated package from your app's `pubspec.yaml`:

   ```yaml
   dependencies:
     weaveffi:
       path: ../generated/dart
   ```

4. Bundle the cdylib per platform:

   - iOS / macOS: ship a Framework or use a `podspec`.
   - Android: place `.so` files under `android/src/main/jniLibs/{abi}/`.
   - Linux / Windows: place next to the executable or on the library
     search path.

## Memory and ownership

- **Strings:** Dart `String` values are converted with
  `toNativeUtf8()`. The wrapper frees the resulting pointer in a
  `finally` block. Returned UTF-8 pointers are copied with
  `toDartString()` and then released with `weaveffi_free_string`.
- **Bytes, lists, and maps:** returned buffers are copied into Dart
  collections, then the producer's allocation is released. String
  elements are freed individually with `weaveffi_free_string` before
  the backing buffer is freed with `weaveffi_free_bytes`.
- **Structs and interfaces:** wrappers hold a `Pointer<Void>`. The
  `dispose()` method calls the corresponding `_destroy` C function.
  Always wrap usage in `try`/`finally`:

  ```dart
  final contact = getContact(id);
  try {
    print(contact.name);
  } finally {
    contact.dispose();
  }
  ```

- **Optionals:** `T?` returns check the native pointer against
  `nullptr` before wrapping; absent optionals become `null`. A boxed
  optional scalar is dereferenced, then the box is freed with
  `weaveffi_free_bytes`.
- **Iterators:** each yielded element is copied (or, for records,
  adopted by its wrapper class), and the iterator handle is destroyed
  exactly once; see [Iterators](#iterators).

## Callbacks and listeners

A `callbacks:` entry in the IDL defines the native function-pointer
type; a `listeners:` entry generates a register/unregister pair around
it. Registration wraps the Dart closure in a `NativeCallable`, hands
its `nativeFunction` pointer to the C ABI, and returns the `int`
subscription id the native side minted:

```dart
// Live listener trampolines by subscription id. Holding the
// NativeCallable here keeps its native thunk alive until unregistered.
final Map<int, NativeCallable> _listenerCallables = {};

/// Registers a OnMessage listener. Returns a subscription id for
/// unregisterMessageListener().
int registerMessageListener(void Function(String message) callback) {
  final callable =
      NativeCallable<_NativeCb_weaveffi_events_OnMessage_fn>.isolateLocal(
          (Pointer<Utf8> message, Pointer<Void> context) {
    callback(message == nullptr ? '' : message.toDartString());
  });
  final id = _weaveffiEventsRegisterMessageListener(callable.nativeFunction, nullptr);
  _listenerCallables[id] = callable;
  return id;
}

/// Unregisters a listener previously registered with registerMessageListener().
void unregisterMessageListener(int id) {
  _weaveffiEventsUnregisterMessageListener(id);
  _listenerCallables.remove(id)?.close();
}
```

- **Lifetime.** The live `NativeCallable` is stored in
  `_listenerCallables` keyed by subscription id; that reference keeps
  the native thunk and the captured closure alive. Unregistering
  removes the entry and `close()`s the callable. The C `void* context`
slot is unused (`nullptr`); the closure travels inside the callable,
so no registry id needs to cross the boundary.
- **Threading.** Listener trampolines are
  `NativeCallable.isolateLocal`, not `.listener`: WeaveFFI listeners
  fire synchronously on the thread calling the producer API (here,
  while `sendMessage` runs), and the argument pointers are only valid
  for that borrow window, so they are converted to Dart values inside
  the callback before the producer frees them. An `isolateLocal`
  callable may only be invoked on the owning isolate's thread, so
  events are delivered during the isolate's own calls into the
  library rather than queued to the event loop.
- **Isolate lifetime.** The generated code never sets
  `keepIsolateAlive = false`, so the `dart:ffi` default applies: a
  registered listener keeps its isolate alive until it is
  unregistered.

## Async support

Functions marked `async: true` return a `Future<T>` backed by the
`_async`-suffixed C launcher. The completion callback is a
`NativeCallable.listener`, which may be invoked from any native
thread: the event is posted to the owning isolate's event loop, where
it completes the `Completer`:

```dart
/// Throws [TaskException] on domain errors.
Future<TaskResult> runTask(String name) {
  final completer = Completer<TaskResult>();
  final namePtr = name.toNativeUtf8();
  late NativeCallable<_NativeAsyncCb_weaveffi_tasks_run_task> callable;
  callable = NativeCallable<_NativeAsyncCb_weaveffi_tasks_run_task>.listener(
      (Pointer<Void> context, Pointer<_WeaveFFIError> err, Pointer<Void> result) {
    try {
      if (err.address != 0 && err.ref.code != 0) {
        final code = err.ref.code;
        final msg = err.ref.message.toDartString();
        _weaveffiErrorClear(err);
        completer.completeError(_mapTaskException(code, msg));
        return;
      }
      completer.complete(TaskResult._(result));
    } catch (e) {
      completer.completeError(e);
    } finally {
      callable.close();
    }
  });
  try {
    _weaveffiTasksRunTaskAsync(namePtr, callable.nativeFunction, nullptr);
  } catch (e) {
    callable.close();
    calloc.free(namePtr);
    rethrow;
  }
  return completer.future.whenComplete(() {
    calloc.free(namePtr);
  });
}
```

The callable is closed in the callback's `finally` (or immediately if
the launch itself throws), so each native trampoline is freed exactly
once; input buffers are released in `whenComplete` once the future
settles. The `dart:async` import is only emitted when the IDL contains
at least one async function.

Result ownership follows the async contract: the callback borrows
string, bytes, list, map, and boxed optional scalar results, so the
callback body deep-copies them into Dart values before it returns and
never frees them (the producer does, after the callback returns).
Object results (records, rich enums, interfaces, including optional
ones) are the exception: the callback receives ownership, and the
wrapper adopts the pointer, as `TaskResult._(result)` does above; its
`dispose()` owns the eventual destroy.

For a callable marked `throws: true`, the completion callback maps an
error through the domain mapper (`_mapTaskException` above,
`_mapKvException` on `Store.compact()`), so the future fails with the
typed exception; a non-throwing async callable can only fail with
`WeaveFFIException` on a producer bug. Async interface methods follow
the same pattern as instance methods returning `Future<T>`.

For functions marked `cancellable: true` the C launcher gains a
`weaveffi_cancel_token*` parameter. The Dart wrapper passes `nullptr`
for it and doesn't expose the token; only the C and C++
targets surface cancellation tokens.

## Iterators

`iter<T>` returns surface as `Iterable<T>` backed by a `sync*`
generator, so they are fully lazy: nothing runs until the consumer
starts iterating, and each element pulls exactly one native `next`
call. Iterating the returned `Iterable` again launches a fresh native
iterator. From the `events` sample:

```dart
/// Return an iterator over all sent messages
///
/// Returns a lazy [Iterable]: elements are pulled from the native
/// iterator one at a time (one native `next` call per element), and
/// iterating the result again launches a fresh native iterator.
///
/// The native iterator handle is destroyed exactly once: eagerly when
/// the iteration completes or fails, or by a GC finalizer if the
/// iteration is abandoned before it is exhausted.
Iterable<String> getMessages() sync* {
  final err = calloc<_WeaveFFIError>();
  final outItem = calloc<Pointer<Utf8>>();
  Pointer<Void> iter = nullptr;
  final anchor = _IteratorLifetime();
  try {
    iter = _weaveffiEventsGetMessages(err);
    _checkError(err);
    _weaveffiEventsGetMessagesIteratorDestroyFinalizer.attach(anchor, iter, detach: anchor);
    while (_weaveffiEventsGetMessagesIteratorNext(iter, outItem, err) != 0) {
      _checkError(err);
      final itemPtr = outItem.value;
      final item = itemPtr.toDartString();
      _weaveffiFreeString(itemPtr);
      yield item;
    }
    _checkError(err);
  } finally {
    if (iter != nullptr) {
      _weaveffiEventsGetMessagesIteratorDestroyFinalizer.detach(anchor);
      _weaveffiEventsGetMessagesIteratorDestroy(iter);
      iter = nullptr;
    }
    calloc.free(outItem);
    calloc.free(err);
  }
}
```

Each yielded string is copied with `toDartString()` and its producer
allocation released with `weaveffi_free_string`; record elements are
adopted by their wrapper class instead. The handle lifecycle covers
early abandonment: the `finally` block runs when the loop exhausts,
when a step fails, or when the consumer stops iterating (Dart closes
the suspended `sync*` frame on `break`). If an iteration is abandoned
without ever resuming the frame, the `_IteratorLifetime` anchor's
`NativeFinalizer` destroys the handle at GC time; an eagerly destroyed
handle detaches first, so the destroy runs exactly once either way.

Errors from the launcher and from each `next` follow the function's
error strategy: the throwing `kvstore` sample's `Store.listKeys`
checks each step with `_checkKvException` and throws the typed
`KvException` subclasses from the step that failed; the non-throwing
`getMessages` throws `WeaveFFIException` only for producer bugs.

## Troubleshooting

- **`Invalid argument(s): Failed to load dynamic library`**: the
  cdylib is not on the search path. Set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the library next to your executable.
- **`UnsupportedError: Unsupported platform`**: the loader maps to
  `darwin`, `linux`, and `windows`. Other platforms (Android, iOS) use
  the Flutter integration where the framework opens the library.
- **`MissingPluginException` in Flutter**: that error is unrelated to
  WeaveFFI; double-check that you depend on the generated package and
  haven't shadowed it with a different `weaveffi` dependency.
- **Strings appear truncated**: Rust strings aren't nul-terminated;
  make sure `toDartString()` is reading the pointer returned from a
  generated getter, not a raw pointer.
