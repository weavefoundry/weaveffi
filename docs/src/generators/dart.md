# Dart

The Dart generator produces a pure-Dart FFI package that wraps the C ABI
using [`dart:ffi`](https://dart.dev/interop/c-interop). It uses
`DynamicLibrary.open` to load the shared library at runtime and
`lookupFunction` to resolve each C symbol. No native compilation step or
code generation tooling (e.g. `ffigen`) is required — the generated `.dart`
file is ready to use.

## Why dart:ffi?

- **Built into the Dart SDK.** `dart:ffi` ships with Dart since 2.6 and is
  the official mechanism for calling native code.
- **Works with Flutter.** The same bindings work in Flutter apps on iOS,
  Android, macOS, Linux, and Windows.
- **No build step.** The generated Dart file is plain Dart — add the package
  as a dependency and import it.
- **Null-safe.** Generated code uses Dart's sound null-safety throughout.

## Generated artifacts

| File | Purpose |
|------|---------|
| `dart/lib/weaveffi.dart` | `dart:ffi` bindings: library loader, typedefs, lookup bindings, wrapper functions, enum/struct classes |
| `dart/pubspec.yaml` | Package metadata (name, SDK constraint, `ffi` dependency) |
| `dart/README.md` | Basic usage instructions |

## dart:ffi approach

All native calls go through a single `DynamicLibrary` instance. For each C
symbol, the generator emits:

1. A **native typedef** describing the C function signature using FFI types
   (`Int32`, `Pointer<Utf8>`, etc.).
2. A **Dart typedef** describing the equivalent Dart signature (`int`,
   `Pointer<Utf8>`, etc.).
3. A `lookupFunction` call that resolves the symbol at load time.
4. A **wrapper function** with idiomatic Dart types (`String`, `bool`, enum
   classes, struct classes) that handles marshalling, calls the looked-up
   function, checks for errors, and converts the result.

Every C function takes a trailing `Pointer<_WeaveffiError>` parameter. The
wrapper allocates this struct via `calloc`, passes it to the native call,
and calls `_checkError` afterward to convert non-zero error codes into a
`WeaveffiException`.

## Generated code examples

Given this IDL definition:

```yaml
version: "0.3.0"
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
          - { name: contact_type, type: ContactType }

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

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32
```

### Library loader

The generated module auto-detects the platform and loads the shared library:

```dart
DynamicLibrary _openLibrary() {
  if (Platform.isMacOS) return DynamicLibrary.open('libweaveffi.dylib');
  if (Platform.isLinux) return DynamicLibrary.open('libweaveffi.so');
  if (Platform.isWindows) return DynamicLibrary.open('weaveffi.dll');
  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');
}

final DynamicLibrary _lib = _openLibrary();
```

### Enums

Enums map to Dart enhanced enums with an `int value` field. Variant names
are converted to lowerCamelCase:

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

Enum parameters are passed as `.value` (an `int` mapped to `Int32`); enum
returns are converted back via `fromValue`.

### Structs (opaque wrapper classes)

Structs are wrapped as Dart classes holding a `Pointer<Void>` to the
Rust-allocated data. A `dispose()` method calls the C ABI destroy function.
Field access is through getters that call the C ABI getter functions:

```dart
/// A contact record
class Contact {
  final Pointer<Void> _handle;
  Contact._(this._handle);

  void dispose() {
    _weaveffiContactsContactDestroy(_handle);
  }

  String get name {
    final err = calloc<_WeaveffiError>();
    try {
      final result = _weaveffiContactsContactGetName(_handle, err);
      _checkError(err);
      return result.toDartString();
    } finally {
      calloc.free(err);
    }
  }

  String? get email {
    final err = calloc<_WeaveffiError>();
    try {
      final result = _weaveffiContactsContactGetEmail(_handle, err);
      _checkError(err);
      if (result == nullptr) return null;
      return result.toDartString();
    } finally {
      calloc.free(err);
    }
  }

  int get age {
    final err = calloc<_WeaveffiError>();
    try {
      final result = _weaveffiContactsContactGetAge(_handle, err);
      _checkError(err);
      return result;
    } finally {
      calloc.free(err);
    }
  }

  ContactType get contactType {
    final err = calloc<_WeaveffiError>();
    try {
      final result = _weaveffiContactsContactGetContactType(_handle, err);
      _checkError(err);
      return ContactType.fromValue(result);
    } finally {
      calloc.free(err);
    }
  }
}
```

### Functions

Each IDL function produces a set of typedefs, a `lookupFunction` binding,
and a top-level wrapper function. String parameters are marshalled to
native UTF-8 via `toNativeUtf8()` and freed in a `finally` block:

```dart
typedef _NativeWeaveffiContactsCreateContact =
    Int64 Function(Pointer<Utf8>, Pointer<Utf8>, Int32, Pointer<_WeaveffiError>);
typedef _DartWeaveffiContactsCreateContact =
    int Function(Pointer<Utf8>, Pointer<Utf8>, int, Pointer<_WeaveffiError>);
final _weaveffiContactsCreateContact = _lib.lookupFunction<
    _NativeWeaveffiContactsCreateContact,
    _DartWeaveffiContactsCreateContact>('weaveffi_contacts_create_contact');

int createContact(String name, String? email, ContactType contactType) {
  final err = calloc<_WeaveffiError>();
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

Contact getContact(int id) {
  final err = calloc<_WeaveffiError>();
  try {
    final result = _weaveffiContactsGetContact(id, err);
    _checkError(err);
    return Contact._(result);
  } finally {
    calloc.free(err);
  }
}

Contact? findContact(int id) {
  final err = calloc<_WeaveffiError>();
  try {
    final result = _weaveffiContactsFindContact(id, err);
    _checkError(err);
    if (result == nullptr) return null;
    return Contact._(result);
  } finally {
    calloc.free(err);
  }
}
```

## Type mapping reference

| IDL type     | Dart type           | Native FFI type        | Dart FFI type        |
|--------------|---------------------|------------------------|----------------------|
| `i32`        | `int`               | `Int32`                | `int`                |
| `u32`        | `int`               | `Uint32`               | `int`                |
| `i64`        | `int`               | `Int64`                | `int`                |
| `f64`        | `double`            | `Double`               | `double`             |
| `bool`       | `bool`              | `Int32`                | `int`                |
| `string`     | `String`            | `Pointer<Utf8>`        | `Pointer<Utf8>`      |
| `bytes`      | `List<int>`         | `Pointer<Uint8>`       | `Pointer<Uint8>`     |
| `handle`     | `int`               | `Int64`                | `int`                |
| `StructName` | `StructName`        | `Pointer<Void>`        | `Pointer<Void>`      |
| `EnumName`   | `EnumName`          | `Int32`                | `int`                |
| `T?`         | `T?`                | same as inner type     | same as inner type   |
| `[T]`        | `List<T>`           | `Pointer<Void>`        | `Pointer<Void>`      |
| `{K: V}`     | `Map<K, V>`         | `Pointer<Void>`        | `Pointer<Void>`      |

Booleans are transmitted as `Int32` (`0`/`1`) because C has no standard
fixed-width boolean type across ABIs. The wrapper converts with
`flag ? 1 : 0` for parameters and `result != 0` for returns.

## Null-safety

Generated code uses Dart's sound null-safety:

- **Optional return types** (`T?`) check the native pointer against
  `nullptr` before wrapping. If null, they return `null`:

```dart
Contact? findContact(int id) {
  // ...
  if (result == nullptr) return null;
  return Contact._(result);
}
```

- **Optional struct fields** (e.g. `string?`) produce nullable getters
  (`String?`) that guard against null pointers:

```dart
String? get email {
  // ...
  if (result == nullptr) return null;
  return result.toDartString();
}
```

- **Non-optional types** are always non-nullable in the generated API.
  A non-optional struct return that receives a null pointer from the C
  layer will surface as a `WeaveffiException` via the error-checking
  mechanism.

## Async support

Functions marked `async: true` in the IDL produce both a synchronous
helper (prefixed with `_`) and a public `Future`-returning wrapper that
runs the FFI call on a separate `Isolate` via `Isolate.run`:

```dart
String _fetchData(int id) {
  final err = calloc<_WeaveffiError>();
  try {
    final result = _weaveffiMathFetchData(id, err);
    _checkError(err);
    return result.toDartString();
  } finally {
    calloc.free(err);
  }
}

Future<String> fetchData(int id) async {
  return await Isolate.run(() => _fetchData(id));
}
```

This keeps the main isolate's event loop responsive while the Rust function
executes. The `dart:isolate` import is only included when the API contains
at least one async function.

## Error handling

Native errors are propagated through a `_WeaveffiError` FFI struct
containing an integer code and a UTF-8 message pointer. After every native
call, `_checkError` inspects the struct and throws `WeaveffiException`
when the code is non-zero:

```dart
final class _WeaveffiError extends Struct {
  @Int32()
  external int code;
  external Pointer<Utf8> message;
}

class WeaveffiException implements Exception {
  final int code;
  final String message;
  WeaveffiException(this.code, this.message);
  @override
  String toString() => 'WeaveffiException($code): $message';
}

void _checkError(Pointer<_WeaveffiError> err) {
  if (err.ref.code != 0) {
    final msg = err.ref.message.toDartString();
    _weaveffiErrorClear(err);
    throw WeaveffiException(err.ref.code, msg);
  }
}
```

Catch errors in consumer code:

```dart
try {
  final contact = getContact(42);
  print(contact.name);
} on WeaveffiException catch (e) {
  print('Error ${e.code}: ${e.message}');
}
```

## Memory management

### Strings

- **Passing strings in:** Dart `String` values are converted to native
  UTF-8 via `toNativeUtf8()` (from `package:ffi`). The resulting pointer
  is freed in a `finally` block via `calloc.free()`.
- **Receiving strings back:** Returned `Pointer<Utf8>` values are decoded
  via `toDartString()`.

### Structs (opaque pointers)

Struct wrappers hold a `Pointer<Void>`. The `dispose()` method calls the
corresponding C ABI `_destroy` function. Callers are responsible for
calling `dispose()` when done:

```dart
final contact = getContact(id);
try {
  print(contact.name);
  print(contact.email ?? '(none)');
} finally {
  contact.dispose();
}
```

## Using in a Flutter project

### 1. Generate bindings

```bash
weaveffi generate --input api.yaml --output generated/ --target dart
```

### 2. Build the Rust shared library

Cross-compile for each Flutter target platform:

```bash
# iOS
cargo build --target aarch64-apple-ios --release

# Android
cargo build --target aarch64-linux-android --release

# macOS
cargo build --target aarch64-apple-darwin --release

# Linux
cargo build --target x86_64-unknown-linux-gnu --release
```

### 3. Add the generated package

Reference the generated package from your Flutter app's `pubspec.yaml`:

```yaml
dependencies:
  weaveffi:
    path: ../generated/dart
```

### 4. Bundle the shared library

Place the compiled shared library where Flutter can find it at runtime:

- **iOS/macOS:** Add as a framework or use a `podspec` to bundle `libweaveffi.dylib`.
- **Android:** Place `.so` files under `android/src/main/jniLibs/{abi}/`.
- **Linux/Windows:** Place next to the executable or on the library search path.

### 5. Import and use

```dart
import 'package:weaveffi/weaveffi.dart';

void main() {
  final handle = createContact('Alice', 'alice@example.com', ContactType.work);
  final contact = getContact(handle);
  print('${contact.name} (${contact.email})');
  print('Total: ${countContacts()}');
  contact.dispose();
}
```

## Build and test (standalone Dart)

### 1. Generate bindings

```bash
weaveffi generate --input api.yaml --output generated/ --target dart
```

### 2. Build the Rust shared library

```bash
cargo build --release -p your_library
```

### 3. Make the shared library findable

**macOS:**
```bash
DYLD_LIBRARY_PATH=../../target/release dart run example/main.dart
```

**Linux:**
```bash
LD_LIBRARY_PATH=../../target/release dart run example/main.dart
```

**Windows:**
Place `weaveffi.dll` in the same directory as your script, or add its
directory to `PATH`.
