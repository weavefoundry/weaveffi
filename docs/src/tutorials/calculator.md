# Calculator end-to-end

## Goal

Take the in-tree `samples/calculator` producer (safe Rust annotated with
`#[weaveffi::module]`), generate bindings for every target, build the cdylib,
and run the calculator from a real consumer (C, Node.js, Swift, then optionally
Kotlin and Wasm). By the end you will have produced bindings, executed them on
at least one host, and seen the same Rust `add(a, b)` answer come back through
three different runtimes, plus the typed `CalcError` surface when you divide
by zero. Along the way you'll extract the sample's IDL to YAML and validate
it, which is the same path a hand-written IDL takes.

## Prerequisites

- Rust toolchain (stable channel) with `cargo` on `PATH`.
- The WeaveFFI CLI (`cargo install weaveffi-cli` or
  `cargo run -p weaveffi-cli --` if you are working in the repo).
- macOS or Linux for the C/Node/Swift steps; Windows works for C and
  Node but the Swift step requires macOS.
- For the optional Kotlin and Wasm steps:
  - Android Studio with the NDK installed.
  - `rustup target add wasm32-unknown-unknown`.

## Step-by-step

### 1. Look at the producer

The whole sample is one annotated module. The `#[weaveffi::error]` enum is
the module's error domain, `div` returns `Result<i32, CalcError>` and so
becomes a `throws` function, and `echo` shows a `String` crossing in both
directions:

```rust
#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain: the codes its throwing functions report.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// division by zero
        DivisionByZero = 1,
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Multiply two integers.
    #[weaveffi::export]
    pub fn mul(a: i32, b: i32) -> i32 {
        a * b
    }

    /// Divide two integers, failing on a zero divisor.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, CalcError> {
        if b == 0 {
            return Err(CalcError::DivisionByZero);
        }
        Ok(a / b)
    }

    /// Echo a string back to the caller.
    #[weaveffi::export]
    pub fn echo(s: String) -> String {
        s
    }
}

weaveffi::export_runtime!();
```

The `Cargo.toml` builds it as a `cdylib` (plus an `rlib` so the unit tests
can call the thunks) and depends on the `weaveffi` facade crate. The
[Producer Macro](../guides/producer-macro.md) guide covers every attribute.

### 2. Extract and validate the IDL

Every generator consumes the IDL, whether it came from annotated Rust or a
YAML file you wrote by hand. Extract the sample's IDL so you can see what the
generators see, then validate it:

```bash
weaveffi extract samples/calculator/src/lib.rs -o calculator.yml
weaveffi validate calculator.yml
```

The second command prints `Validation passed` and a one-line summary
(`1 modules, 4 functions, 0 structs, 0 enums`). The extracted file is the
complete IDL for this module:

```yaml
version: 0.9.0
modules:
- name: calculator
  functions:
  - name: add
    params:
    - name: a
      type: i32
    - name: b
      type: i32
    return: i32
    doc: Add two integers.
  - name: mul
    params:
    - name: a
      type: i32
    - name: b
      type: i32
    return: i32
    doc: Multiply two integers.
  - name: div
    params:
    - name: a
      type: i32
    - name: b
      type: i32
    return: i32
    doc: Divide two integers, failing on a zero divisor.
    throws: true
  - name: echo
    params:
    - name: s
      type: string
    return: string
    doc: Echo a string back to the caller.
  errors:
    name: CalcError
    codes:
    - name: DivisionByZero
      code: 1
      message: division by zero
      doc: division by zero
```

If you'd rather author the IDL first and write the Rust afterwards, start
from this file: `weaveffi generate calculator.yml -o generated` produces the
same bindings as the next step, provided the same `weaveffi.toml` sits beside
the YAML (otherwise the package name falls back to the file stem, `calc` for
`calc.yml`, and the loaders look for `libcalc.dylib`). The
[IDL reference](../reference/idl.md) documents every field, and the
[Configuration](../guides/config.md) guide covers `weaveffi.toml`.

### 3. Generate every target

Point the generator at the annotated source; the sample's `weaveffi.toml`
beside it supplies the package identity:

```bash
weaveffi generate samples/calculator/src/lib.rs -o generated
```

The command finishes with
`Generated artifacts in generated (config: samples/calculator/weaveffi.toml)`.
The output appears under `generated/`, one directory per target. The three
this tutorial exercises:

- `generated/c`: C header (`weaveffi.h`) and convenience C file
- `generated/swift`: SwiftPM System Library (`CCalculator`) and Swift wrapper
  (`Calculator`)
- `generated/node`: N-API addon source, JS loader, `binding.gyp`, and
  `types.d.ts`

The rest (`cpp`, `kotlin`, `wasm`, `python`, `dotnet`, `dart`, `go`, `ruby`)
follow the same pattern; the [generator pages](../generators/README.md) cover
each one.

### 4. Build the Rust sample

```bash
cargo build -p calculator
```

The cdylib lands in `target/debug/`:

- macOS: `libcalculator.dylib`
- Linux: `libcalculator.so`
- Windows: `calculator.dll`

### 5. Run a C consumer

Write a minimal `main.c` that calls through the generated header. `add` is
non-throwing, so its error slot only trips on a panic or a marshalling
failure; `div` is declared `throws`, so a zero divisor fills `out_err` with
the typed `CalcError` code:

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

### 6. Run a Node consumer

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

### 7. Run a Swift consumer (macOS / Linux)

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
also links `libweaveffi`, so this reuses the symlink from step 6):

```bash
swiftc \
  -I generated/swift/Sources/CCalculator \
  -L target/debug \
  -Xlinker -rpath -Xlinker target/debug \
  generated/swift/Sources/Calculator/Calculator.swift main.swift -o calc_swift
./calc_swift
```

On Linux, export `LD_LIBRARY_PATH=target/debug` before running so the loader
resolves the `libweaveffi.so` alias.

### 8. Optional: Kotlin and Wasm

- Open `generated/kotlin` in Android Studio and build the `calculator`
  library module (that's the `rootProject.name` in the generated
  `settings.gradle.kts`). The [Kotlin tutorial](kotlin.md) walks through
  cross-compiling the cdylib for Android and wiring the AAR into an app.
- For Wasm, run
  `cargo build -p calculator --target wasm32-unknown-unknown --release` and
  load `target/wasm32-unknown-unknown/release/calculator.wasm` with
  `generated/wasm/weaveffi_wasm.js`; the generated `README.md` beside it has
  a loading example.

## Verification

You should see the same calculator output from each consumer. Concretely:

- `weaveffi validate` reports `Validation passed` for the extracted IDL.
- The C consumer prints `2 + 3 = 5` and the typed division error.
- The Node one-liner prints the sum, then `DivisionByZeroError`
  from the thrown error class.
- The Swift binary prints the same arithmetic, catches the thrown
  `CalcError`, and exits cleanly.

For fuller consumers that exercise interfaces, callback interfaces, and async
functions, see the `conformance/` directory: each `conformance/<target>/`
file is a runnable program against the richer samples (contacts, events,
kvstore, shapes), and `conformance/run.sh` builds and runs them all.

If the host cannot find the cdylib, you will see
`dyld: Library not loaded` (macOS) or `error while loading shared
libraries` (Linux). Re-export `DYLD_LIBRARY_PATH` /
`LD_LIBRARY_PATH` and rerun.

## Cleanup

```bash
rm -rf generated/ calculator.yml main.c main.swift calc_c calc_swift
rm -f target/debug/libweaveffi.dylib   # the link alias from step 6
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
  [Swift iOS](swift.md), [Kotlin](kotlin.md),
  [Python](python.md), or [Node.js](node.md).
