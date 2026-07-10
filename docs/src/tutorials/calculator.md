# Calculator end-to-end

## Goal

Take the in-tree `samples/calculator` producer (safe Rust annotated with
`#[weaveffi::module]`), generate bindings for every target, build the cdylib,
and run the calculator from a real consumer (C, Node.js, Swift, then optionally
Android and Wasm). By the end you will have produced bindings, executed them on
at least one host, and seen the same Rust `add(a, b)` answer come back through
three different runtimes, plus the typed `CalcError` surface when you divide
by zero.

## Prerequisites

- Rust toolchain (stable channel) with `cargo` on `PATH`.
- The WeaveFFI CLI (`cargo install weaveffi-cli` or
  `cargo run -p weaveffi-cli --` if you are working in the repo).
- macOS or Linux for the C/Node/Swift steps; Windows works for C and
  Node but the Swift step requires macOS.
- For the optional Android and Wasm steps:
  - Android Studio with the NDK installed.
  - `rustup target add wasm32-unknown-unknown`.

## Step-by-step

### 1. Generate every target

Point the generator at the annotated source (the `calculator.yml` IDL still
works too, and produces the same bindings):

```bash
weaveffi generate samples/calculator/src/lib.rs -o generated
```

The output appears under `generated/`, one directory per target. The three
this tutorial exercises:

- `generated/c`: C header (`weaveffi.h`) and convenience C file
- `generated/swift`: SwiftPM System Library (`CWeaveFFI`) and Swift wrapper (`WeaveFFI`)
- `generated/node`: N-API addon source, JS loader, and `.d.ts`

The rest (`cpp`, `android`, `wasm`, `python`, `dotnet`, `dart`, `go`, `ruby`)
follow the same pattern; the [generator pages](../generators/README.md) cover
each one.

### 2. Build the Rust sample

```bash
cargo build -p calculator
```

The cdylib lands in `target/debug/`:

- macOS: `libcalculator.dylib`
- Linux: `libcalculator.so`
- Windows: `calculator.dll`

### 3. Run a C consumer

Write a minimal `main.c` that calls through the generated header. `add` is
non-throwing, so its error slot only trips on a poisoned call; `div` is
declared `throws`, so a zero divisor fills `out_err` with the typed
`CalcError` code:

```c
#include <stdio.h>
#include "weaveffi.h"

int main(void) {
    weaveffi_error err = {0};
    printf("2 + 3 = %d\n", weaveffi_calculator_add(2, 3, &err));

    weaveffi_calculator_div(1, 0, &err);
    if (err.code == weaveffi_calculator_CalcError_DivisionByZero) {
        printf("div(1, 0) failed: %s\n", err.message);
        weaveffi_error_clear(&err);
    }
    return 0;
}
```

Compile and run it from the repo root (on Linux, replace
`DYLD_LIBRARY_PATH` with `LD_LIBRARY_PATH`):

```bash
cc -I generated/c main.c -L target/debug -lcalculator -o calc_c
DYLD_LIBRARY_PATH=target/debug ./calc_c
```

You should see `2 + 3 = 5` followed by `div(1, 0) failed: division by zero`.

### 4. Run a Node consumer

The generated `binding.gyp` links against `libweaveffi`, so give the sample
cdylib that name with a symlink, then build the addon in place (`npm install`
runs `node-gyp rebuild`; `LIBRARY_PATH` tells the linker where to find the
alias):

```bash
ln -sf libcalculator.dylib target/debug/libweaveffi.dylib   # .so on Linux
cd generated/node
LIBRARY_PATH="$PWD/../../target/debug" npm install
```

Then call the wrapper. Names are camelCase with the module prefix stripped,
and the throwing `div` raises a typed error class:

```bash
DYLD_LIBRARY_PATH=../../target/debug node -e "
const calc = require('./index.js');
console.log('2 + 3 =', calc.add(2, 3));
try { calc.div(1, 0); } catch (e) { console.log(e.name + ':', e.message); }
"
```

This prints `2 + 3 = 5` and `DivisionByZeroError: (1) division by zero`.

### 5. Run a Swift consumer (macOS / Linux)

Write a `main.swift` at the repo root. The wrapper exposes the module as
a `Calculator` enum namespace, and `div` is `throws`:

```swift
print("2 + 3 = \(Calculator.add(a: 2, b: 3))")
do {
    _ = try Calculator.div(a: 1, b: 0)
} catch {
    print("div(1, 0) failed: \(error.localizedDescription)")
}
```

Compile the generated wrapper together with your `main.swift` (the module map
also links `libweaveffi`, so this reuses the symlink from step 4):

```bash
swiftc \
  -I generated/swift/Sources/CWeaveFFI \
  -L target/debug \
  -Xlinker -rpath -Xlinker target/debug \
  generated/swift/Sources/WeaveFFI/WeaveFFI.swift main.swift -o calc_swift
./calc_swift
```

On Linux, export `LD_LIBRARY_PATH=target/debug` before running so the loader
resolves the `libweaveffi.so` alias.

### 6. Optional: Android and Wasm

- Open `generated/android` in Android Studio and build the `:weaveffi`
  AAR. Combine with the steps in the
  [Android tutorial](android.md).
- For Wasm, run
  `cargo build --target wasm32-unknown-unknown --release` and load the
  `.wasm` file with `generated/wasm/weaveffi_wasm.js`.

## Verification

You should see the same calculator output from each consumer. Concretely:

- The C consumer prints `2 + 3 = 5` and the typed division error.
- The Node one-liner prints the sum, then `DivisionByZeroError`
  from the thrown error class.
- The Swift binary prints the same arithmetic, catches the thrown
  `CalcError`, and exits cleanly.

For fuller consumers that exercise interfaces, callbacks, and async
functions, see the `conformance/` directory: each `conformance/<target>/`
file is a runnable program against the richer samples (contacts, events,
kvstore, shapes), and `conformance/run.sh` builds and runs them all.

If the host cannot find the cdylib, you will see
`dyld: Library not loaded` (macOS) or `error while loading shared
libraries` (Linux). Re-export `DYLD_LIBRARY_PATH` /
`LD_LIBRARY_PATH` and rerun.

## Cleanup

```bash
rm -rf generated/ main.c main.swift calc_c calc_swift
rm -f target/debug/libweaveffi.dylib   # the link alias from step 4
cargo clean -p calculator
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
