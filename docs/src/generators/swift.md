# Swift

The Swift generator emits a SwiftPM System Library (`CWeaveFFI`) that
references the generated C header via a `module.modulemap`, and a thin
Swift module (`WeaveFFI`) that wraps the C API with Swift types and
`throws`-based error handling.

## Generated artifacts

- `generated/swift/Package.swift`
- `generated/swift/Sources/CWeaveFFI/module.modulemap` — C module map pointing at the generated header
- `generated/swift/Sources/WeaveFFI/WeaveFFI.swift` — thin Swift wrapper

## Try the example app

### macOS

```bash
cargo build -p calculator

cd examples/swift
swiftc \
  -I ../../generated/swift/Sources/CWeaveFFI \
  -L ../../target/debug -lcalculator \
  -Xlinker -rpath -Xlinker ../../target/debug \
  Sources/App/main.swift -o .build/debug/App

DYLD_LIBRARY_PATH=../../target/debug .build/debug/App
```

### Linux

```bash
cargo build -p calculator

cd examples/swift
swiftc \
  -I ../../generated/swift/Sources/CWeaveFFI \
  -L ../../target/debug -lcalculator \
  -Xlinker -rpath -Xlinker ../../target/debug \
  Sources/App/main.swift -o .build/debug/App

LD_LIBRARY_PATH=../../target/debug .build/debug/App
```

## Integration via SwiftPM

In a real app, add the System Library as a dependency and link it with your
target. The `CWeaveFFI` module map provides header linkage; import `WeaveFFI`
in your Swift code for the ergonomic wrapper.
