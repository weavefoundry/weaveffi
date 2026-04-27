# SQLite Contacts Dart Example

A pure-Dart project that consumes the generated `package:weaveffi/weaveffi.dart`
bindings for the `samples/sqlite-contacts` sample.

It demonstrates async/await over the SQLite-backed CRUD API:

- `await createContact(...)`
- `await findContact(...)`
- `await updateContact(...)`
- `await countContacts(...)`
- `await deleteContact(...)`

Each returned `Contact` owns a native handle. The example keeps those handles
in a small list and calls `dispose()` in a `finally` block so they release
deterministically even if an awaited operation throws.

## Prerequisites

- Dart SDK >= 3.0.0 (`dart --version`)
- A recent Rust toolchain

## 1. Build the sqlite-contacts cdylib

From the repo root:

```bash
cargo build -p sqlite-contacts
```

This produces:

- macOS: `target/debug/libsqlite_contacts.dylib`
- Linux: `target/debug/libsqlite_contacts.so`
- Windows: `target\debug\sqlite_contacts.dll`

## 2. Regenerate the Dart bindings for the SQLite contacts IDL

The Dart generator writes its output to `generated/dart/`, which this example
depends on via a path reference in `pubspec.yaml`:

```bash
cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o generated --target dart
```

## 3. Make the sqlite-contacts cdylib discoverable as `libweaveffi.*`

The generated `DynamicLibrary.open` call looks for the library named after
the configured C prefix (`weaveffi` by default), not `sqlite_contacts`. Create
a symlink (or copy) so the dynamic loader finds it:

### macOS

```bash
ln -sf "$PWD/target/debug/libsqlite_contacts.dylib" \
       "$PWD/target/debug/libweaveffi.dylib"
```

### Linux

```bash
ln -sf "$PWD/target/debug/libsqlite_contacts.so" \
       "$PWD/target/debug/libweaveffi.so"
```

### Windows (PowerShell, developer command prompt)

```powershell
Copy-Item target\debug\sqlite_contacts.dll target\debug\weaveffi.dll
```

## 4. Run it

From `examples/dart/sqlite-contacts`:

### macOS

```bash
cd examples/dart/sqlite-contacts
dart pub get
DYLD_LIBRARY_PATH=../../../target/debug dart run bin/main.dart
```

### Linux

```bash
cd examples/dart/sqlite-contacts
dart pub get
LD_LIBRARY_PATH=../../../target/debug dart run bin/main.dart
```

### Windows

Add the directory containing `weaveffi.dll` to `PATH` before running:

```powershell
cd examples\dart\sqlite-contacts
dart pub get
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
dart run bin\main.dart
```

Expected output:

```text
=== Dart SQLite Contacts Example ===

Created #1 Alice
Created #2 Bob

Found #1: Alice <alice@example.com> (Active)
Updated Alice's email: true
Refetched #1: Alice <alice@new.com> (Active)

Totals: all=2 active=2
Deleted Bob: true
Remaining: 1
```
