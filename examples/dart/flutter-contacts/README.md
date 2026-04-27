# Flutter Contacts Example

A minimal Flutter app that consumes the generated
`package:weaveffi/weaveffi.dart` bindings for the `samples/contacts` sample and
renders the native contacts list with Material widgets.

The app exercises:

- `createContact` / `countContacts`
- `listContacts`
- `Contact.dispose()` in a `finally` block for deterministic native handle
  cleanup

## Prerequisites

- Flutter SDK (`flutter --version`)
- A recent Rust toolchain

## 1. Build the contacts cdylib

From the repo root:

```bash
cargo build -p contacts
```

This produces:

- macOS: `target/debug/libcontacts.dylib`
- Linux: `target/debug/libcontacts.so`
- Windows: `target\debug\contacts.dll`

## 2. Regenerate the Dart bindings

The app depends on `generated/dart/` via a path dependency:

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target dart
```

## 3. Make the contacts cdylib discoverable as `libweaveffi.*`

The generated `DynamicLibrary.open` call looks for the library named after the
configured C prefix (`weaveffi` by default), not `contacts`.

### macOS

```bash
ln -sf "$PWD/target/debug/libcontacts.dylib" \
       "$PWD/target/debug/libweaveffi.dylib"
```

### Linux

```bash
ln -sf "$PWD/target/debug/libcontacts.so" \
       "$PWD/target/debug/libweaveffi.so"
```

### Windows (PowerShell, developer command prompt)

```powershell
Copy-Item target\debug\contacts.dll target\debug\weaveffi.dll
```

## 4. Run it

From `examples/dart/flutter-contacts`:

### macOS

```bash
cd examples/dart/flutter-contacts
flutter pub get
DYLD_LIBRARY_PATH=../../../target/debug flutter run
```

### Linux

```bash
cd examples/dart/flutter-contacts
flutter pub get
LD_LIBRARY_PATH=../../../target/debug flutter run
```

### Windows

Add the directory containing `weaveffi.dll` to `PATH` before running:

```powershell
cd examples\dart\flutter-contacts
flutter pub get
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
flutter run
```

## Optional CI

This example is intentionally optional in CI because Flutter is not required to
work on WeaveFFI itself. CI can call:

```bash
examples/dart/flutter-contacts/tool/flutter_ci.sh
```

The script exits successfully without building when the Flutter SDK is not
available. When Flutter is available, it regenerates the Dart bindings, runs
`flutter analyze`, runs `flutter test`, and builds a Flutter bundle.
