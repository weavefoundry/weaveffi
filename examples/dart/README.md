# Dart End-to-End Example

Loads the calculator and contacts cdylibs at runtime via `dart:ffi`
`DynamicLibrary.open` and exercises a representative slice of the C
ABI: `add`, `create_contact`, `list_contacts`, `delete_contact`. Prints
`OK` and exits 0 on success.

## Prerequisites

- Dart SDK 3.0+

## Run

```bash
cargo build -p calculator -p contacts

dart pub get --directory examples/dart

WEAVEFFI_LIB=target/debug/libcalculator.dylib \
CONTACTS_LIB=target/debug/libcontacts.dylib \
  dart run examples/dart/main.dart
```

On Linux replace `.dylib` with `.so`. Or run via `examples/run_all.sh`.
