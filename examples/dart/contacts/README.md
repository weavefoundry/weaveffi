# Contacts Dart Example

A pure-Dart project that consumes the generated `package:weaveffi/weaveffi.dart`
bindings for the `samples/contacts` sample.

It exercises:

- `createContact` / `countContacts`
- `listContacts` — returns a `List<Contact>`, where each element owns a
  native handle
- `getContact` — returns a single `Contact`
- `deleteContact`
- Dart's equivalent of RAII: every `Contact` exposes a `dispose()` method
  that calls `weaveffi_contacts_Contact_destroy` on the underlying handle.
  A `Finalizer` is attached as a safety net so forgotten handles are still
  released when the `Contact` becomes unreachable, but explicit `dispose()`
  in a `finally` block is the recommended, deterministic pattern.

## Prerequisites

- Dart SDK >= 3.0.0 (`dart --version`)
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

## 2. Regenerate the Dart bindings for the contacts IDL

The Dart generator writes its output to `generated/dart/`, which this example
depends on via a path reference in `pubspec.yaml`. Regenerate against
`samples/contacts/contacts.yml`:

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target dart
```

## 3. Make the contacts cdylib discoverable as `libweaveffi.*`

The generated `DynamicLibrary.open` call looks for the library named after
the configured C prefix (`weaveffi` by default), not `contacts`. Create a
symlink (or copy) so the dynamic loader finds it:

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

From `examples/dart/contacts`:

### macOS

```bash
cd examples/dart/contacts
dart pub get
DYLD_LIBRARY_PATH=../../../target/debug dart run bin/main.dart
```

### Linux

```bash
cd examples/dart/contacts
dart pub get
LD_LIBRARY_PATH=../../../target/debug dart run bin/main.dart
```

### Windows

Add the directory containing `weaveffi.dll` to `PATH` before running:

```powershell
cd examples\dart\contacts
dart pub get
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
dart run bin\main.dart
```

Expected output:

```
=== Dart Contacts Example ===

Created contact #1
Created contact #2

Total: 2 contacts

All contacts:
  [1] Alice Smith <alice@example.com> (Personal)
  [2] Bob Jones (Work)

Get contact #1:
  [1] Alice Smith <alice@example.com> (Personal)

Deleted contact #2: true
Total: 1 contacts
```
