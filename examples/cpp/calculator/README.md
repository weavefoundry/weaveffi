# Calculator C++ Example

A CMake project that consumes the generated C++ wrappers for the
`samples/calculator` sample.

It exercises:

- `weaveffi::calculator_add` / `calculator_mul` / `calculator_div` /
  `calculator_echo`.
- The generated `weaveffi::WeaveFFIError` exception type (raised when
  dividing by zero).

## Prerequisites

- CMake 3.16+
- A C++17 compiler (clang, gcc, or MSVC)
- A recent Rust toolchain

## 1. Build the calculator cdylib

From the repo root:

```bash
cargo build -p calculator
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libcalculator.dylib`
- Linux: `target/debug/libcalculator.so`
- Windows: `target\debug\calculator.dll`

## 2. Regenerate the C++ bindings for the calculator IDL

The checked-in `generated/cpp/` may track a different sample; regenerate
against `samples/calculator/calculator.yml` so `weaveffi.hpp` exposes the
calculator functions:

```bash
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated --target cpp
```

## 3. Configure and build this example

### macOS / Linux

```bash
cd examples/cpp/calculator
cmake -S . -B build
cmake --build build
```

### Windows (Visual Studio)

```powershell
cd examples\cpp\calculator
cmake -S . -B build
cmake --build build --config Debug
```

By default, the CMake project looks for the cdylib in
`../../../target/debug`. Pass `-DCALCULATOR_LIB_DIR=/absolute/path` to
override (for example when building `--release`):

```bash
cmake -S . -B build -DCALCULATOR_LIB_DIR=$PWD/../../../target/release
```

## 4. Run it

The generated `weaveffi_cpp` CMake target is an INTERFACE library, so the
calculator cdylib is loaded dynamically at runtime.

### macOS

On macOS the CMake project already embeds a build rpath pointing at
`CALCULATOR_LIB_DIR`, so you can just run the binary:

```bash
./build/calculator
```

If you moved the cdylib elsewhere, either set `DYLD_LIBRARY_PATH` or
rebuild:

```bash
DYLD_LIBRARY_PATH=../../../target/debug ./build/calculator
```

### Linux

```bash
LD_LIBRARY_PATH=../../../target/debug ./build/calculator
```

### Windows

Add the directory containing `calculator.dll` to `PATH` before running:

```powershell
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
.\build\Debug\calculator.exe
```

Expected output:

```
=== C++ Calculator Example ===

add(3, 4) = 7
mul(5, 6) = 30
div(10, 2) = 5
echo("hello") = hello

div(1, 0) threw WeaveFFIError 2: division by zero
```
