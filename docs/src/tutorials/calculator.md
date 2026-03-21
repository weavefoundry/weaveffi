# Tutorial: Calculator end-to-end

This tutorial uses the included `samples/calculator` crate and shows how to
generate artifacts and run platform examples.

## 1) Generate artifacts

```bash
weaveffi generate samples/calculator/calculator.yml -o generated
```

This writes headers and templates under `generated/`:

- `generated/c` — C header and convenience C file
- `generated/swift` — SwiftPM System Library (`CWeaveFFI`) and Swift wrapper (`WeaveFFI`)
- `generated/android` — Kotlin wrapper + JNI shims + Gradle skeleton
- `generated/node` — N-API addon loader + `.d.ts`
- `generated/wasm` — minimal loader stub

## 2) Build the Rust sample

```bash
cargo build -p calculator
```

This produces a shared library:

- macOS: `target/debug/libcalculator.dylib`
- Linux: `target/debug/libcalculator.so`

## 3) Run the C example

### macOS

```bash
cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
DYLD_LIBRARY_PATH=../../target/debug ./c_example
```

### Linux

```bash
cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
LD_LIBRARY_PATH=../../target/debug ./c_example
```

## 4) Run the Node example

### macOS

```bash
cp target/debug/libindex.dylib generated/node/index.node
cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

### Linux

```bash
cp target/debug/libindex.so generated/node/index.node
cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

## 5) Try Swift (macOS)

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

On Linux, replace `DYLD_LIBRARY_PATH` with `LD_LIBRARY_PATH`.

## 6) Android and WASM

- Open `generated/android` in Android Studio and build the `:weaveffi` AAR.
- Build for WASM: `cargo build --target wasm32-unknown-unknown --release` and
  load with `generated/wasm/weaveffi_wasm.js`.
