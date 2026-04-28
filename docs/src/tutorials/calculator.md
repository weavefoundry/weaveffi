# Calculator end-to-end

## Goal

Take the in-tree `samples/calculator` IDL, generate bindings for every
target, build the cdylib, and run the calculator from a real consumer
(C, Node.js, Swift, then optionally Android and WASM). By the end you
will have produced bindings, executed them on at least one host, and
seen the same Rust `add(a, b)` answer come back through three different
runtimes.

## Prerequisites

- Rust toolchain (stable channel) with `cargo` on `PATH`.
- The WeaveFFI CLI (`cargo install weaveffi-cli` or
  `cargo run -p weaveffi-cli --` if you are working in the repo).
- macOS or Linux for the C/Node/Swift steps; Windows works for C and
  Node but the Swift step requires macOS.
- For the optional Android and WASM steps:
  - Android Studio with the NDK installed.
  - `rustup target add wasm32-unknown-unknown`.

## Step-by-step

### 1. Generate every target

```bash
weaveffi generate samples/calculator/calculator.yml -o generated
```

The output appears under `generated/`:

- `generated/c` — C header and convenience C file
- `generated/swift` — SwiftPM System Library (`CWeaveFFI`) and Swift wrapper (`WeaveFFI`)
- `generated/android` — Kotlin wrapper, JNI shims, and Gradle skeleton
- `generated/node` — N-API loader and `.d.ts`
- `generated/wasm` — minimal WASM loader

### 2. Build the Rust sample

```bash
cargo build -p calculator
```

The cdylib lands in `target/debug/`:

- macOS: `libcalculator.dylib`
- Linux: `libcalculator.so`
- Windows: `calculator.dll`

### 3. Run the C example

macOS:

```bash
cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
DYLD_LIBRARY_PATH=../../target/debug ./c_example
```

Linux:

```bash
cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
LD_LIBRARY_PATH=../../target/debug ./c_example
```

### 4. Run the Node example

macOS:

```bash
cp target/debug/libindex.dylib generated/node/index.node
cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

Linux:

```bash
cp target/debug/libindex.so generated/node/index.node
cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

### 5. Run the Swift example (macOS / Linux)

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

On Linux replace `DYLD_LIBRARY_PATH` with `LD_LIBRARY_PATH`.

### 6. Optional: Android and WASM

- Open `generated/android` in Android Studio and build the `:weaveffi`
  AAR. Combine with the steps in the
  [Android tutorial](android.md).
- For WASM, run
  `cargo build --target wasm32-unknown-unknown --release` and load the
  `.wasm` file with `generated/wasm/weaveffi_wasm.js`.

## Verification

You should see the same calculator output from each consumer, e.g.
`2 + 3 = 5`. Concretely:

- The C example prints `2 + 3 = 5` (or whatever expression
  `examples/c/main.c` exercises) without any
  `weaveffi: error` messages.
- `npm start` exits with code `0` and prints the calculator results
  followed by the `Done.` banner.
- The Swift binary launches, prints the same arithmetic, and exits
  cleanly.

If the host cannot find the cdylib, you will see
`dyld: Library not loaded` (macOS) or `error while loading shared
libraries` (Linux). Re-export `DYLD_LIBRARY_PATH` /
`LD_LIBRARY_PATH` and rerun.

## Cleanup

```bash
rm -rf generated/
cargo clean -p calculator
rm -rf examples/c/c_example examples/swift/.build
```

The `generated/` directory is safe to delete and recreate; nothing
else in the repository depends on its contents.

## Next steps

- Walk through the per-target details in
  [Generators](../generators/README.md).
- Read the [Memory Ownership](../guides/memory.md) and
  [Error Handling](../guides/errors.md) guides for the contracts
  every consumer must follow.
- Try a target-specific tutorial:
  [Swift iOS](swift.md), [Android](android.md),
  [Python](python.md), or [Node.js](node.md).
