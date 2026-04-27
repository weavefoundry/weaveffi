# Getting Started

This guide walks you through installing WeaveFFI, defining an API, generating
multi-language bindings, implementing the Rust library, and calling it from C.

## Prerequisites

You need the [Rust toolchain](https://rustup.rs/) (stable channel) installed.
Verify with:

```bash
rustc --version
cargo --version
```

## 1) Install WeaveFFI

Install the CLI from crates.io:

```bash
cargo install weaveffi-cli
```

This puts the `weaveffi` binary on your `PATH`.

## 2) Create a new project

Scaffold a starter project:

```bash
weaveffi new my-project
cd my-project
```

This creates a `my-project/` directory containing:

- `weaveffi.yml` — an example API definition with `add`, `mul`, and `echo`
  functions
- `README.md` — quick-start notes

## 3) Define your API

Open `weaveffi.yml` and replace its contents with an API that has a struct and
a function:

```yaml
version: "0.3.0"
modules:
  - name: math
    structs:
      - name: Point
        fields:
          - { name: x, type: f64 }
          - { name: y, type: f64 }
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
```

The IDL supports primitives (`i32`, `f64`, `bool`, `string`, `bytes`, `handle`),
optionals (`string?`), and lists (`[i32]`). See the
[IDL Schema](reference/idl.md) reference for the full specification.

## 4) Generate bindings

Run the generator to produce bindings for all targets:

```bash
weaveffi generate weaveffi.yml -o generated --scaffold
```

The `--scaffold` flag also emits a `scaffold.rs` with Rust FFI stubs you can
use as a starting point. The output tree looks like:

```text
generated/
├── c/          # C header + convenience stubs
├── swift/      # SwiftPM package + Swift wrapper
├── android/    # Kotlin JNI wrapper + Gradle skeleton
├── node/       # N-API loader + TypeScript types
├── wasm/       # WASM loader stub
└── scaffold.rs # Rust FFI function stubs
```

## 5) Examine the generated output

### C header (`generated/c/weaveffi.h`)

The C generator produces an opaque struct with lifecycle functions and getters,
plus a module-level function. Every exported function takes an `out_err`
parameter for error reporting:

```c
typedef struct weaveffi_math_Point weaveffi_math_Point;

weaveffi_math_Point* weaveffi_math_Point_create(
    double x, double y, weaveffi_error* out_err);
void weaveffi_math_Point_destroy(weaveffi_math_Point* ptr);
double weaveffi_math_Point_get_x(const weaveffi_math_Point* ptr);
double weaveffi_math_Point_get_y(const weaveffi_math_Point* ptr);

int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);
```

### Swift wrapper (`generated/swift/Sources/WeaveFFI/WeaveFFI.swift`)

Structs become classes that own an `OpaquePointer` and free it on `deinit`.
Module functions are grouped under a Swift enum namespace:

```swift
public class Point {
    let ptr: OpaquePointer
    deinit { weaveffi_math_Point_destroy(ptr) }

    public var x: Double { weaveffi_math_Point_get_x(ptr) }
    public var y: Double { weaveffi_math_Point_get_y(ptr) }
}

public enum Math {
    public static func add(a: Int32, b: Int32) throws -> Int32 { ... }
}
```

### TypeScript types (`generated/node/types.d.ts`)

Structs become interfaces with mapped types. Functions use the IR name
directly (no module prefix):

```typescript
export interface Point {
  x: number;
  y: number;
}

// module math
export function add(a: number, b: number): number
```

## 6) Implement the Rust library

The generated `scaffold.rs` contains `todo!()` stubs for every function.
Create a Rust library crate and fill in the implementations.

**Cargo.toml:**

```toml
[package]
name = "my-math"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
weaveffi-abi = { version = "0.1" }
```

**src/lib.rs** — implement the `add` function (struct lifecycle omitted for
brevity):

```rust
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_math_add(
    a: i32,
    b: i32,
    out_err: *mut weaveffi_error,
) -> i32 {
    abi::error_set_ok(out_err);
    a + b
}

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr);
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len);
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err);
}
```

Key points:

- Every exported function uses `#[no_mangle]` and `extern "C"`.
- `out_err` must always be cleared on success with `abi::error_set_ok`.
- On error, call `abi::error_set(out_err, code, message)` and return a
  zero/null value.
- The library must export `weaveffi_free_string`, `weaveffi_free_bytes`, and
  `weaveffi_error_clear` for the runtime.

Build with:

```bash
cargo build
```

This produces a shared library (`libmy_math.dylib` on macOS,
`libmy_math.so` on Linux).

## 7) Build and test with C

Write a small C program that calls your library:

**main.c:**

```c
#include <stdio.h>
#include "weaveffi.h"

int main(void) {
    struct weaveffi_error err = {0};

    int32_t sum = weaveffi_math_add(3, 4, &err);
    if (err.code) {
        printf("error: %s\n", err.message);
        weaveffi_error_clear(&err);
        return 1;
    }
    printf("add(3, 4) = %d\n", sum);

    return 0;
}
```

Compile, link, and run:

```bash
# macOS
cc -I generated/c main.c -L target/debug -lmy_math -o my_example
DYLD_LIBRARY_PATH=target/debug ./my_example

# Linux
cc -I generated/c main.c -L target/debug -lmy_math -o my_example
LD_LIBRARY_PATH=target/debug ./my_example
```

Expected output:

```text
add(3, 4) = 7
```

## Next steps

- Run `weaveffi doctor` to check which platform toolchains are available.
- See the [Calculator tutorial](tutorials/calculator.md) for a full end-to-end
  walkthrough including Swift and Node.js.
- Read the [IDL Schema](reference/idl.md) reference for all supported types
  and features.
- Explore the [Generators](generators/README.md) section for target-specific
  details.

## Checking a single target

`weaveffi doctor` runs every toolchain check it knows about. To narrow it
down to a single language target, pass `--target {name}`:

```bash
weaveffi doctor --target dart
weaveffi doctor --target cpp
weaveffi doctor --target go
weaveffi doctor --target ruby
weaveffi doctor --target dotnet
weaveffi doctor --target python
weaveffi doctor --target swift
weaveffi doctor --target android
weaveffi doctor --target node
weaveffi doctor --target wasm
```

Only checks whose `applies_to` set contains the chosen target (plus the
required Rust toolchain, which always runs) are executed. When `--target`
is set the command exits with a non-zero status if any of those checks
failed, making it scriptable in CI:

```bash
if ! weaveffi doctor --target dart; then
  echo "Dart toolchain not ready" >&2
  exit 1
fi
```

For machine-readable output (handy for piping into `jq` or aggregating
results across CI matrices), use `--format json`:

```bash
weaveffi doctor --target ruby --format json | jq '.[] | select(.ok == false)'
```

Each entry has `id`, `name`, `ok`, `version`, `hint`, and `applies_to` fields.
