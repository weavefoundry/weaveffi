# Dart

## Overview

The Dart target produces a pure-Dart FFI package that wraps the C ABI
using [`dart:ffi`](https://dart.dev/interop/c-interop). It opens the
shared library with `DynamicLibrary.open` and resolves each symbol via
`lookupFunction`. There is no native compilation step or `ffigen` run
required — the generated `.dart` file is ready to import.

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
| `bool`       | `bool`              | `Int32`                | `int`                |
| `string`     | `String`            | `Pointer<Utf8>`        | `Pointer<Utf8>`      |
| `bytes`      | `List<int>`         | `Pointer<Uint8>`       | `Pointer<Uint8>`     |
| `handle`     | `int`               | `Int64`                | `int`                |
| `StructName` | `StructName`        | `Pointer<Void>`        | `Pointer<Void>`      |
| `EnumName`   | `EnumName`          | `Int32`                | `int`                |
| `T?`         | `T?`                | same as inner type     | same as inner type   |
| `[T]`        | `List<T>`           | `Pointer<Void>`        | `Pointer<Void>`      |
| `{K: V}`     | `Map<K, V>`         | `Pointer<Void>`        | `Pointer<Void>`      |

Booleans cross as `Int32` (`0`/`1`) and the wrapper converts both ways.

## Example IDL → generated code

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

  void dispose() { _weaveffiContactsContactDestroy(_handle); }

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
}
```

Each function emits a native typedef, Dart typedef, lookup, and
top-level wrapper:

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
```

## Build instructions

Standalone Dart:

1. Generate the bindings:

   ```bash
   weaveffi generate --input api.yaml --output generated/ --target dart
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
  `finally` block. Returned UTF-8 pointers are decoded with
  `toDartString()`.
- **Structs:** wrappers hold a `Pointer<Void>`. The `dispose()` method
  calls the corresponding `_destroy` C function. Always wrap usage in
  `try`/`finally`:

  ```dart
  final contact = getContact(id);
  try {
    print(contact.name);
  } finally {
    contact.dispose();
  }
  ```

- **Optionals:** `T?` returns check the native pointer against
  `nullptr` before wrapping; absent struct optionals become `null`.

## Async support

Functions marked `async: true` produce a synchronous helper plus a
public `Future`-returning wrapper that runs the FFI call on a separate
isolate via `Isolate.run`:

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

The `dart:isolate` import is only included when the IDL contains at
least one async function. When the IDL marks the function
`cancel: true`, the wrapper forwards Dart cancellation tokens to the
underlying `weaveffi_cancel_token`.

## Troubleshooting

- **`Invalid argument(s): Failed to load dynamic library`** — the
  cdylib is not on the search path. Set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the library next to your executable.
- **`UnsupportedError: Unsupported platform`** — the loader maps to
  `darwin`, `linux`, and `windows`. Other platforms (Android, iOS) use
  the Flutter integration where the framework opens the library.
- **`MissingPluginException` in Flutter** — that error is unrelated to
  WeaveFFI; double-check that you depend on the generated package and
  haven't shadowed it with a different `weaveffi` dependency.
- **Strings appear truncated** — Rust strings are not nul-terminated;
  make sure `toDartString()` is reading the pointer returned from a
  generated getter, not a raw pointer.
