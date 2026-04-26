# Calculator sample

A minimal WeaveFFI sample that exposes four functions through a stable C ABI:

| Function | Signature | Notes |
|----------|-----------|-------|
| `add`  | `(i32, i32) -> i32` | Adds two integers. |
| `mul`  | `(i32, i32) -> i32` | Multiplies two integers. |
| `div`  | `(i32, i32) -> i32` | Divides two integers; sets `weaveffi_error` on division by zero. |
| `echo` | `(string) -> string` | Round-trips a UTF-8 string; caller must free the result with `weaveffi_free_string`. |

The sample demonstrates:

- A Rust `cdylib` (`crate-type = ["cdylib", "rlib"]`) that implements the
  C ABI entry points (`weaveffi_calculator_*`) using helpers from
  [`weaveffi-abi`](../../crates/weaveffi-abi).
- The [`calculator.yml`](calculator.yml) IDL that WeaveFFI consumes to
  generate idiomatic wrappers for every supported target language.
- Error propagation through the `weaveffi_error` out-parameter.
- Heap-allocated string ownership transferred across the FFI boundary and
  freed via `weaveffi_free_string`.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated

# A single target
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated --target c

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated --target c,cpp,swift
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

Each target writes into its own subdirectory under `-o`, for example
`generated/c/weaveffi.h`, `generated/swift/Package.swift`,
`generated/python/weaveffi/__init__.py`, and so on.

## Build the cdylib

From the repo root:

```bash
cargo build -p calculator
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libcalculator.dylib`
- Linux: `target/debug/libcalculator.so`
- Windows: `target\debug\calculator.dll`

## Run the C example end-to-end

This walkthrough builds a small C driver against the generated header and
runs it against the `calculator` cdylib. All commands are run from the
repo root.

1. Generate the C header and build the cdylib:

```bash
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated --target c
cargo build -p calculator
```

2. Save the following driver as `calc_demo.c`:

```c
#include <stdio.h>
#include <string.h>
#include "generated/c/weaveffi.h"

int main(void) {
    weaveffi_error err = {0};

    int32_t sum = weaveffi_calculator_add(3, 4, &err);
    if (err.code) { printf("add error: %s\n", err.message); weaveffi_error_clear(&err); return 1; }
    printf("add(3,4) = %d\n", sum);

    int32_t prod = weaveffi_calculator_mul(5, 6, &err);
    if (err.code) { printf("mul error: %s\n", err.message); weaveffi_error_clear(&err); return 1; }
    printf("mul(5,6) = %d\n", prod);

    int32_t q = weaveffi_calculator_div(10, 2, &err);
    if (err.code) { printf("div error: %s\n", err.message); weaveffi_error_clear(&err); return 1; }
    printf("div(10,2) = %d\n", q);

    const char* msg = "hello";
    const char* echoed = weaveffi_calculator_echo(
        (const uint8_t*)msg, strlen(msg), &err);
    if (err.code) { printf("echo error: %s\n", err.message); weaveffi_error_clear(&err); return 1; }
    printf("echo(hello) = %s\n", echoed);
    weaveffi_free_string(echoed);

    (void)weaveffi_calculator_div(1, 0, &err);
    if (err.code) { printf("div error expected: %s\n", err.message); weaveffi_error_clear(&err); }

    return 0;
}
```

3. Compile and run:

```bash
# Compile
cc -I . calc_demo.c -L target/debug -lcalculator -o calc_demo

# Run (point the dynamic loader at target/debug)
# macOS
DYLD_LIBRARY_PATH=target/debug ./calc_demo
# Linux
LD_LIBRARY_PATH=target/debug ./calc_demo
```

Expected output:

```
add(3,4) = 7
mul(5,6) = 30
div(10,2) = 5
echo(hello) = hello
div error expected: division by zero
```
