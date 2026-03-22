## Contacts Swift Example

1. Build the contacts library (from repo root):

```bash
cargo build -p contacts
```

2. Compile and run directly against the C system module:

```bash
cd examples/swift/contacts

mkdir -p .build/debug
swiftc \
  -I ../../../generated/swift/Sources/CWeaveFFI \
  -L ../../../target/debug -lcontacts \
  -Xlinker -rpath -Xlinker ../../../target/debug \
  Sources/App/main.swift -o .build/debug/App

DYLD_LIBRARY_PATH=../../../target/debug .build/debug/App
```
