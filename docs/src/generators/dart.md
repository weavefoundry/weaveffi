# Dart

## Overview

The Dart target produces a pure-Dart FFI package that wraps the C ABI
(revision 2) using [`dart:ffi`](https://dart.dev/interop/c-interop). It
opens the shared library with `DynamicLibrary.open` and resolves each
symbol via `lookupFunction`. There's no native compilation step or
`ffigen` run required; the generated `.dart` file is ready to import.

Records and rich enums are plain Dart classes (a sealed hierarchy for a
rich enum) that cross the ABI as value buffers. Interfaces are
reference-counted wrapper classes with `dispose()` and a
`NativeFinalizer` backstop. Callback interfaces are abstract classes the
consumer implements, bound through `NativeCallable.isolateLocal`
trampolines. Async functions return `Future<T>`, and `iter<T>` returns
are lazy `Iterable<T>`s backed by `sync*` generators.

## What gets generated

| File | Purpose |
|------|---------|
| `dart/lib/weaveffi.dart` | `dart:ffi` bindings: loader, ABI check, typedefs, lookups, wrappers, record/enum classes |
| `dart/pubspec.yaml` | Package metadata and `package:ffi` dependency |
| `dart/README.md` | Basic usage instructions |

The library verifies the producer's ABI revision when it's first
loaded, throwing a `StateError` on mismatch:

```dart
const int _abiVersion = 2;
```

## Type mapping

| IDL type     | Dart type           | Native FFI type        |
|--------------|---------------------|------------------------|
| `i8`, `i16`, `i32`, `i64` | `int`  | `Int8`, `Int16`, `Int32`, `Int64` |
| `u8`, `u16`, `u32`, `u64` | `int`  | `Uint8`, `Uint16`, `Uint32`, `Uint64` |
| `f32`, `f64` | `double`            | `Float`, `Double`      |
| `bool`       | `bool`              | `Int32`                |
| `string`     | `String`            | `Pointer<Utf8>`        |
| `bytes`      | `List<int>`         | `Pointer<Uint8>` + `Size` |
| `StructName` | `StructName` (plain class) | value buffer (`Pointer<Uint8>` + `Size`) |
| `EnumName` (plain) | `EnumName` (enhanced enum) | `Int32`      |
| `EnumName` (rich)  | `EnumName` (sealed class hierarchy) | value buffer |
| `InterfaceName` | `InterfaceName` (wrapper class) | `Pointer<Void>` |
| `InterfaceName?` | `InterfaceName?`  | `Pointer<Void>` (`nullptr` for `null`) |
| `CallbackName` | `CallbackName` (abstract class to extend) | `Pointer<Void>` ctx + vtable pointer |
| `T?`         | `T?`                | value buffer           |
| `[T]`        | `List<T>`           | value buffer           |
| `{K: V}`     | `Map<K, V>`         | value buffer           |
| `iter<T>`    | `Iterable<T>` (lazy) | `Pointer<Void>`       |

Buffered types cross the boundary serialized in the
[value-buffer format](../reference/value-buffers.md); the library
carries a private `_BufferWriter`/`_BufferReader` pair plus one
`_pack*`/`_unpack*` routine per record and rich enum. Objects nested
inside a buffered value travel as object tokens (see
[Objects](#objects-interfaces)). Booleans cross as `Int32` (`0`/`1`)
and the wrapper converts both ways.

### 64-bit integers and floats

Dart's `int` is a signed 64-bit integer, so `i64` round-trips exactly.
`u64` is carried as its two's-complement bit pattern: `u64::MAX` arrives
as `-1` and `2^63` as `-9223372036854775808`, both across `Uint64` FFI
slots and inside value buffers (`writeUint64`/`readUint64`). Apply
`.toUnsigned(64)` or `BigInt.from(v).toUnsigned(64)` when you need the
arithmetic value. `f32`/`f64` map to `double`; the `codec` conformance
consumer verifies NaN, both infinities, and `-0.0` survive a round trip
bit-for-bit.

## Example IDL and generated code

```yaml
version: "0.9.0"
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

Structs are plain Dart value classes: one final typed field per IDL
field plus a constructor with named arguments. They declare no C symbols
(no destroy, no getters) and there's nothing to dispose:

```dart
/// A contact record
class Contact {
  final String name;
  final String? email;
  final int age;

  Contact({required this.name, this.email, required this.age});
}
```

Each function emits a native typedef, Dart typedef, lookup, and
top-level wrapper. Buffered parameters are packed and staged in native
memory the producer borrows for the call; buffered returns are copied,
freed, and decoded. From the `kvstore` sample's cross-module `getStats`:

```dart
typedef _NativeWeaveffiKvStatsGetStats = Pointer<Uint8> Function(Pointer<Void>, Pointer<Size>, Pointer<_WeaveFFIError>);
typedef _DartWeaveffiKvStatsGetStats = Pointer<Uint8> Function(Pointer<Void>, Pointer<Size>, Pointer<_WeaveFFIError>);
final _weaveffiKvStatsGetStats = _lib.lookupFunction<
    _NativeWeaveffiKvStatsGetStats, _DartWeaveffiKvStatsGetStats>('weaveffi_kv_stats_get_stats');

/// Throws [KvException] on domain errors.
Stats getStats(Store store) {
  final outLen = calloc<Size>();
  final err = calloc<_WeaveFFIError>();
  try {
    final result = _weaveffiKvStatsGetStats(store._handle, outLen, err);
    _checkKvException(err);
    final n = outLen.value;
    final data = _copyNativeBytes(result, n);
    if (result != nullptr) _weaveffiFreeBytes(result, n);
    final reader = _BufferReader(data);
    final decoded = _unpackStats(reader);
    reader.expectEnd();
    return decoded;
  } finally {
    calloc.free(outLen);
    calloc.free(err);
  }
}
```

Wrapper names are lowerCamelCase with the IDL module prefix stripped by
default (a `kv.open_store` function would surface as `openStore`, not
`kvOpenStore`); the C symbols keep their full names. Set
`strip_module_prefix: false` in the Dart generator config (or under
`[global]`) to keep module-prefixed wrapper names.

## Typed errors

The package defines `WeaveFFIException` with `code` and `message` fields
and the four runtime trap codes as constants:

```dart
class WeaveFFIException implements Exception {
  /// The producer reported an untyped error.
  static const int genericCode = -1;
  /// The producer panicked; [message] carries the panic text.
  static const int panicCode = -2;
  /// An argument could not be lifted by the producer.
  static const int marshalCode = -3;
  /// A Dart callback-interface implementation threw; [message] carries the
  /// exception's text.
  static const int foreignCode = -4;
  final int code;
  final String message;
  WeaveFFIException(this.code, this.message);
  @override
  String toString() => '$runtimeType($code): $message';
}
```

A module's error domain adds an exception subclass named by replacing
the trailing `Error` stem with `Exception` (`KvError` becomes
`KvException`) plus one subclass per code, and a mapper that falls back
to `WeaveFFIException` for codes outside the domain. From the `kvstore`
sample:

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
```

Only callables marked `throws: true` in the IDL check their error slot
with `_checkKvException` (their doc comments read
`Throws [KvException] on domain errors.`); catching
`KeyNotFoundException` or `KvException` works as usual. A callable
without `throws` uses the generic `_checkError`, which throws
`WeaveFFIException` if the producer misbehaves. Both copy the message
out and release the slot with `weaveffi_error_clear` before throwing.

An error code that declares payload `fields:` carries them serialized in
the error's payload buffer; the mapper decodes them into typed fields on
the exception before the buffer is released.

### Runtime error codes

| Code | Constant | Meaning | Where it surfaces |
|------|----------|---------|-------------------|
| `-1` | `genericCode` | The producer reported an error without a declared code. | Thrown as `WeaveFFIException`. |
| `-2` | `panicCode` | The Rust implementation panicked; the export macros and the async spawner catch the unwind. | Thrown as `WeaveFFIException`, or fails the awaited `Future`. |
| `-3` | `marshalCode` | Malformed input at the boundary (invalid UTF-8, a truncated value buffer, a bad enum discriminant). | Thrown as `WeaveFFIException`. |
| `-4` | `foreignCode` | A callback-interface method implemented in Dart threw. | Thrown as `WeaveFFIException` from the producer call that invoked the callback (see [Callback interfaces](#callback-interfaces)). |

There's no non-throwing call path in Dart: a non-throwing callable whose
error slot comes back non-zero still throws `WeaveFFIException`. Using a
disposed wrapper is reported separately as `StateError`.

## Objects (interfaces)

An `interfaces:` entry becomes a class that adopts one strong reference
to a reference-counted producer object. From the `kvstore` sample
(trimmed):

```dart
final _weaveffiKvStoreDestroyFinalizer = NativeFinalizer(
    _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('weaveffi_kv_Store_destroy'));

class Store implements Finalizable {
  final Pointer<Void> _ptr;
  bool _disposed = false;

  /// Adopts one strong reference to the native object.
  Store._(this._ptr) {
    _weaveffiKvStoreDestroyFinalizer.attach(this, _ptr, detach: this);
  }

  /// The borrowed native pointer for the duration of a call.
  Pointer<Void> get _handle {
    if (_disposed) throw StateError('Store used after dispose()');
    return _ptr;
  }

  /// Mints a second strong reference (the interface's `_clone` symbol) for
  /// an object token written into a value buffer.
  Pointer<Void> _cloneRef() => _weaveffiKvStoreClone(_handle);

  /// Releases this instance's native reference. Safe to call more than
  /// once; the native object is dropped when its last reference (this
  /// one, any other wrapper's, or the producer's) is released.
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _weaveffiKvStoreDestroyFinalizer.detach(this);
    _weaveffiKvStoreDestroy(_ptr);
  }

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
}
```

- **Construction.** A constructor named `new` renders as an unnamed
  `factory` (the `events` sample's `EventBus()`); other constructors
  become named factories (`Store.open(path)`). Methods are
  lowerCamelCase instance methods, statics are static methods, and
  deprecated members carry `@Deprecated`.
- **Disposal.** `dispose()` releases this wrapper's reference through
  the `_destroy` symbol. It's idempotent, and a `NativeFinalizer`
  attached at construction destroys the reference at GC time if the
  wrapper was never disposed (disposal detaches it first, so the destroy
  runs exactly once). The producer object itself is dropped only when
  the last reference anywhere is released.
- **Use after dispose.** Every call reads the private `_handle` getter,
  which throws `StateError('Store used after dispose()')` on a disposed
  wrapper, whether it's the receiver, a parameter, or a field of a record
  being packed.
- **Copies mint new references.** Methods returning the receiver or an
  existing object (`share()`, `fork()`) return a fresh strong reference
  adopted into a new wrapper; disposing one never affects another.

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

### Nullable objects, and objects inside values

An `Interface?` parameter passes `nullptr` for `null`, and an
`Interface?` return maps `nullptr` to `null`:

```dart
Store? larger(Store? other) {
  final err = calloc<_WeaveFFIError>();
  try {
    final result = _weaveffiKvStoreLarger(_handle, other?._handle ?? nullptr, err);
    _checkError(err);
    return result == nullptr ? null : Store._(result);
  } finally {
    calloc.free(err);
  }
}
```

Objects inside records, lists, maps, and optionals travel as 8-byte
object tokens in the value buffer. Writing a token mints a new strong
reference with `_cloneRef()`; reading one adopts the reference into a
fresh wrapper. From the `StoreInfo` record (`store: Store`,
`mirror: Store?`):

```dart
void _packStoreInfo(_BufferWriter w, StoreInfo v) {
  w.writeString(v.label);
  w.writeUint64(v.store._cloneRef().address);
  final t0 = v.mirror;
  if (t0 == null) {
    w.writeOptionFlag(false);
  } else {
    w.writeOptionFlag(true);
    w.writeUint64(t0._cloneRef().address);
  }
  w.writeInt64(v.count);
}

StoreInfo _unpackStoreInfo(_BufferReader r) {
  return StoreInfo(
    label: r.readString(),
    store: Store._(Pointer<Void>.fromAddress(r.readUint64())),
    mirror: (r.readOptionFlag() ? Store._(Pointer<Void>.fromAddress(r.readUint64())) : null),
    count: r.readInt64(),
  );
}
```

Lists of objects work the same way in both directions
(`Store.openMany(paths)` returns `List<Store>`,
`Store.totalCount(stores, extra)` takes one); each wrapper in a returned
list owns its own reference and must be disposed (or left to the
finalizer) individually. Iterators over objects adopt one reference per
step, and async functions returning an object adopt the pointer inside
the completion callback before completing the `Future`.

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style enum surfaces as a Dart `enum` and crosses as an
`Int32`; a rich enum instead becomes an idiomatic sealed class
hierarchy: a sealed base class plus one subclass per variant named
`{Enum}{Variant}`, each carrying that variant's fields. Rich enums
declare no C symbols; values cross the ABI serialized in value buffers
as an `i32` tag followed by the active variant's fields.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`, the generator emits:

```dart
/// An algebraic shape (sum type with associated data)
sealed class Shape {
  const Shape();
}

/// The empty shape
class ShapeEmpty extends Shape {}

/// A circle with a radius
class ShapeCircle extends Shape {
  /// Radius in points
  final double radius;

  ShapeCircle({required this.radius});
}

/// A labeled shape with a small count
class ShapeLabeled extends Shape {
  final String label;
  final int count;

  ShapeLabeled({required this.label, required this.count});
}
```

Construct variants directly and match on the sealed hierarchy with an
exhaustive `switch`:

```dart
final circle = ShapeCircle(radius: 2.0);
final labeled = ShapeLabeled(label: 'unit', count: 3);

switch (circle) {
  case ShapeCircle(:final radius):
    print(radius);                     // 2.0
  case _:
    break;
}

print(describe(circle));               // packs the buffer via the C ABI
final bigger = scale(circle, 3.0);     // returns a new Shape value
```

Values are plain Dart data: there's no native handle and nothing to
dispose. The generated `_packShape`/`_unpackShape` helpers write and
read the tagged wire format; variant fields of interface type follow the
object token rules above.

## Callback interfaces

A `callback_interfaces:` entry becomes an abstract class the consumer
extends (or implements) and passes wherever the API takes that type.
From the `kvstore` sample:

```dart
/// Object arguments are owned by the implementation: call `dispose()` on
/// them (or let the finalizer run) when done. A thrown exception is reported
/// to the producer, which aborts the call it was making; the original Dart
/// caller then observes a [WeaveFFIException] with [WeaveFFIException.foreignCode]
/// carrying the exception's text.
///
/// Dart limitation: methods are bound with `NativeCallable.isolateLocal`, so
/// the producer may only invoke them on the thread of the isolate that
/// passed the callback. That holds when the producer calls them
/// synchronously during a call from Dart. A producer that invokes a method
/// from another thread (an async task, a background worker) is unsupported:
/// a `NativeCallable.listener` cannot return a value or read borrowed
/// arguments after the native frame returns.
abstract class EvictionListener {
  /// An entry left the store. Returns whether the listener wants to keep
  /// receiving notifications; `false` detaches it.
  bool onEvict(Entry entry, EvictionReason reason);
}
```

```dart
class Auditor extends wv.EvictionListener {
  @override
  bool onEvict(wv.Entry entry, wv.EvictionReason reason) {
    print('${entry.key}: $reason');
    return true;
  }
}

store.setEvictionListener(Auditor());
```

Behind the class is one process-wide static vtable per callback
interface, allocated once and never freed, whose method slots are
`NativeCallable.isolateLocal` trampolines and whose `free` slot is a
`NativeCallable.listener`. Passing an implementation parks it in a
handle table under an integer key; the key (widened to a pointer)
crosses as `ctx`, so the producer never holds a Dart object:

```dart
final Map<int, Object> _callbackTable = {};
int _nextCallbackKey = 1;

Pointer<Void> _registerCallback(Object impl) {
  final key = _nextCallbackKey++;
  _callbackTable[key] = impl;
  return Pointer<Void>.fromAddress(key);
}

// Vtable entry `weaveffi_kv_EvictionListener_vtable.on_evict`.
bool _evictionListenerVtOnEvict(Pointer<Void> ctx, Pointer<Uint8> entryPtr, int entryLen, int reason, Pointer<_WeaveFFIError> outErr) {
  try {
    final entryData = _copyNativeBytes(entryPtr, entryLen);
    final entryReader = _BufferReader(entryData);
    final entryValue = _unpackEntry(entryReader);
    entryReader.expectEnd();
    final impl = _callbackFor(ctx) as EvictionListener;
    return impl.onEvict(entryValue, EvictionReason.fromValue(reason));
  } catch (e) {
    _foreignError(outErr, e);
    return false;
  }
}

void _evictionListenerVtFree(Pointer<Void> ctx) {
  _callbackTable.remove(ctx.address);
}

final Pointer<_EvictionListenerVtableStruct> _EvictionListenerVtable = () {
  final vt = calloc<_EvictionListenerVtableStruct>();
  vt.ref.onEvict = _pinCallable(NativeCallable<_NativeEvictionListenerVtOnEvict>.isolateLocal(
      _evictionListenerVtOnEvict, exceptionalReturn: false));
  vt.ref.free = _pinCallable(NativeCallable<_NativeEvictionListenerVtFree>.listener(
      _evictionListenerVtFree));
  return vt;
}();
```

- **Lifetime.** The implementation stays in `_callbackTable` (and
  therefore alive) exactly as long as the producer may call it; the
  vtable's `free` removes it when the producer drops its last reference.
  A producer that retains the implementation (a store's eviction
  listener) keeps it alive across calls; one that doesn't (the `events`
  sample's `routeOnce`) frees it before returning. `free` is a
  `.listener` callable, so it may safely arrive from any thread; the
  removal is posted to the isolate's event loop.
- **Argument ownership.** Borrowed strings and buffers are copied into
  Dart values before the method runs, so the implementation may keep
  them. An object passed to a callback method is owned by the
  implementation: the trampoline adopts it into a new wrapper
  (`impl.onAttached(EventBus._(bus))` in the `events` sample), and the
  implementation should `dispose()` it when done (or let the finalizer
  run).
- **Return values.** A method's return value is converted back to its C
  representation (`bool` as `Bool`, a plain enum as its `value`, a
  record as a value buffer the producer frees).
- **Exceptions.** An exception escaping a method never unwinds through
  the C frame. `_foreignError` writes `foreignCode` (-4) with
  `error.toString()` into the producer's error slot, and the trampoline
  returns the `exceptionalReturn` default; the producer aborts the call
  in progress, and the original Dart caller sees `WeaveFFIException`
  with `code == -4`. For a callable marked `throws`, the domain mapper
  falls through to `WeaveFFIException` (so `on KvException` doesn't
  catch it but `on WeaveFFIException` does). The isolate is never taken
  down.
- **Threads (the important caveat).** Method trampolines are
  `NativeCallable.isolateLocal`, which can only run on the thread of the
  isolate that created them. That's exactly right when the producer
  calls back synchronously during a call from Dart (the `kvstore`
  eviction listener fires inside `delete()`/`get()`). A producer that
  invokes a callback method from another thread (an async task, a
  background worker, a timer) is unsupported and will crash the
  process; `NativeCallable.listener` can't be used instead because it
  returns before the method runs, so it could neither return a value
  nor read borrowed arguments. Keep such producers on the C, C++,
  Swift, Kotlin, Node, .NET, Go, Python, or Ruby targets.
- **Isolate lifetime.** The vtable callables have `keepIsolateAlive`
  cleared, so an idle isolate can still exit while a producer holds a
  callback implementation.

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

3. Make the cdylib findable at runtime. `WEAVEFFI_LIBRARY` may point at
   an exact file; otherwise:

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

## Packaging

`weaveffi package --target dart` emits the Dart package under `dart/`
with a loader that tries the bundled libraries first, and copies each
supplied desktop binary to `dart/native/<platform-id>/` (`macos-arm64`,
`macos-x64`, `linux-x64`, `linux-arm64`, `windows-x64`). The packaged
loader still honours `WEAVEFFI_LIBRARY` first, then probes every
bundled slice for the running OS, then the bare system library name. Android and
`wasm32` binaries have no slot in the loader and are skipped; bundle
them through the Flutter mechanisms above. See
[Packaging](../guides/packaging.md) for the shared workflow.

## Memory and ownership

- **Strings:** Dart `String` values are converted with `toNativeUtf8()`
  and freed in a `finally` block. Returned UTF-8 pointers are copied
  with `toDartString()` and then released with `weaveffi_free_string`.
- **Bytes:** copied into native memory for the call and freed after;
  returned buffers are copied into Dart lists, then the producer's
  allocation is released with `weaveffi_free_bytes`.
- **Buffered values (records, rich enums, optionals, lists, maps):**
  parameters are packed into a value buffer that the producer borrows
  for the duration of the call; returns are copied into Dart memory,
  released with `weaveffi_free_bytes`, and decoded with the generated
  `_unpack*` helper. Object tokens written into a buffer are fresh
  strong references the producer owns; tokens read out are adopted into
  wrappers.
- **Interfaces:** one strong reference per wrapper, released by
  `dispose()` with a `NativeFinalizer` backstop.
- **Callback implementations:** held in `_callbackTable` until the
  producer calls the vtable's `free`.
- **Iterators:** each yielded element is copied (or decoded from a value
  buffer that's then freed), and the iterator handle is destroyed
  exactly once; see [Iterators](#iterators).

## Async support

Functions marked `async: true` return a `Future<T>` backed by the
`_async`-suffixed C launcher. The completion callback is a
`NativeCallable.listener`, which may be invoked from any native thread:
the event is posted to the owning isolate's event loop, where it
completes the `Completer`. From the `kvstore` sample's `Store.compact`:

```dart
/// Throws [KvException] on domain errors.
Future<int> compact() {
  final completer = Completer<int>();
  late NativeCallable<_NativeAsyncCb_weaveffi_kv_Store_compact> callable;
  callable = NativeCallable<_NativeAsyncCb_weaveffi_kv_Store_compact>.listener((Pointer<Void> context, Pointer<_WeaveFFIError> err, int result) {
    try {
      if (err.address != 0 && err.ref.code != 0) {
        final code = err.ref.code;
        final msg = err.ref.message.toDartString();
        final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);
        _weaveffiErrorFree(err);
        completer.completeError(_mapKvException(code, msg, payload));
        return;
      }
      completer.complete(result);
    } catch (e) {
      completer.completeError(e);
    } finally {
      callable.close();
    }
  });
  try {
    _weaveffiKvStoreCompactAsync(_handle, nullptr, callable.nativeFunction, nullptr);
  } catch (e) {
    callable.close();
    rethrow;
  }
  return completer.future;
}
```

The callable is closed in the callback's `finally` (or immediately if
the launch itself throws), so each native trampoline is freed exactly
once; input buffers (a `publishLater(topic, text)` call's string
arguments, say) are released in a `whenComplete` once the future
settles. The `dart:async` import is only emitted when the IDL contains
at least one async function.

Result ownership follows the async contract: the callback owns the
string, bytes, and buffered results it receives (records, rich enums,
optionals, lists, and maps arrive as a `(resultPtr, resultLen)` pair),
so the callback body copies them into Dart values and then releases them
with `_weaveffiFreeString` or `_weaveffiFreeBytes`. Consumer ownership
is what makes `NativeCallable.listener` safe here: the listener defers
the callback body to the isolate's event loop, past the native
callback's return, so a producer-freed buffer would already dangle by
the time it ran. A reported error is heap-boxed and released with
`_weaveffiErrorFree` after its fields are copied. An owned interface
result transfers ownership too: the wrapper adopts the pointer, and its
`dispose()` owns the eventual destroy.

For a callable marked `throws: true`, the completion callback maps an
error through the domain mapper (`_mapKvException` above), so the future
fails with the typed exception; a non-throwing async callable can only
fail with `WeaveFFIException` on a producer bug. A panic inside the
spawned future surfaces as `panicCode` (-2). Async interface methods
follow the same pattern as instance methods returning `Future<T>`; the
receiver's `_handle` is read (throwing `StateError` if disposed) before
the launcher runs.

For functions marked `cancellable: true` the C launcher gains a
`weaveffi_cancel_token*` parameter. The Dart wrapper passes `nullptr`
for it and doesn't expose the token; only the C and C++ targets surface
cancellation tokens.

## Iterators

`iter<T>` returns surface as `Iterable<T>` backed by a `sync*`
generator, so they are fully lazy: nothing runs until the consumer
starts iterating, and each element pulls exactly one native `next` call.
Iterating the returned `Iterable` again launches a fresh native
iterator. From the `kvstore` sample's `Store.listKeys` (argument
packing elided):

```dart
Iterable<String> listKeys(String? prefix) sync* {
  // ... pack the `String?` prefix into a staged value buffer ...
  final err = calloc<_WeaveFFIError>();
  final outItem = calloc<Pointer<Utf8>>();
  Pointer<Void> iter = nullptr;
  final anchor = _IteratorLifetime();
  try {
    iter = _weaveffiKvStoreListKeys(_handle, prefixPtr, prefixBuf.length, err);
    _checkKvException(err);
    _weaveffiKvStoreListKeysIteratorDestroyFinalizer.attach(anchor, iter, detach: anchor);
    while (_weaveffiKvStoreListKeysIteratorNext(iter, outItem, err) != 0) {
      _checkKvException(err);
      final itemPtr = outItem.value;
      final item = itemPtr.toDartString();
      _weaveffiFreeString(itemPtr);
      yield item;
    }
    _checkKvException(err);
  } finally {
    if (iter != nullptr) {
      _weaveffiKvStoreListKeysIteratorDestroyFinalizer.detach(anchor);
      _weaveffiKvStoreListKeysIteratorDestroy(iter);
      iter = nullptr;
    }
    calloc.free(outItem);
    calloc.free(prefixPtr);
    calloc.free(err);
  }
}
```

Each yielded string is copied with `toDartString()` and its producer
allocation released with `weaveffi_free_string`; a buffered element is
decoded from its value buffer and released with `weaveffi_free_bytes`
instead; an object element is adopted into a new wrapper. The handle
lifecycle covers early abandonment: the `finally` block runs when the
loop exhausts, when a step fails, or when the consumer stops iterating
(Dart closes the suspended `sync*` frame on `break`). If an iteration is
abandoned without ever resuming the frame, the `_IteratorLifetime`
anchor's `NativeFinalizer` destroys the handle at GC time; an eagerly
destroyed handle detaches first, so the destroy runs exactly once either
way.

Errors from the launcher and from each `next` follow the function's
error strategy: the throwing `listKeys` checks each step with
`_checkKvException` and throws the typed `KvException` subclasses from
the step that failed; a non-throwing iterator throws `WeaveFFIException`
only for producer bugs. Because the generator is lazy, the launch error
is thrown on the first `moveNext()`, not when the method returns.

## Known limitations

- Callback-interface methods only work when the producer calls them
  synchronously on the thread of the isolate that passed the
  implementation; invocation from another producer thread is
  unsupported.
- `u64` values above `2^63 - 1` arrive as negative `int`s (two's
  complement); convert with `toUnsigned(64)` if needed.
- Async cancellation doesn't propagate: `cancellable: true` tokens are
  not exposed, and an abandoned `Future` leaves the native operation
  running.
- `Iterable<T>` results relaunch the native iterator on each
  re-iteration; the producer sees a fresh call.
- The plain `generate` output relies on the dynamic loader (or
  `WEAVEFFI_LIBRARY`) to find the library; only `weaveffi package`
  bundles desktop slices, and mobile slices go through Flutter.

## Troubleshooting

- **`Invalid argument(s): Failed to load dynamic library`**: the cdylib
  is not on the search path. Set `WEAVEFFI_LIBRARY`,
  `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`, or copy the library next to
  your executable.
- **`StateError: WeaveFFI ABI mismatch`**: the library was built by a
  different `weaveffi` release than the bindings. Regenerate the bindings
  and rebuild the library together.
- **`StateError: Store used after dispose()`**: a disposed wrapper was
  used as a receiver, a parameter, or a record field. Keep the wrapper
  alive for as long as the object is in use.
- **`WeaveFFIException(-4): ...`**: a callback-interface method you
  implemented threw; the message is the exception's `toString()`.
- **Crash or "Cannot invoke native callback outside an isolate"**: the
  producer called a callback-interface method from a thread other than
  the isolate's. That producer can't be used from Dart; see the
  threading caveat above.
- **`UnsupportedError: Unsupported platform`**: the loader maps to
  `darwin`, `linux`, and `windows`. Other platforms (Android, iOS) use
  the Flutter integration where the framework opens the library.
- **`MissingPluginException` in Flutter**: that error is unrelated to
  WeaveFFI; double-check that you depend on the generated package and
  haven't shadowed it with a different `weaveffi` dependency.
