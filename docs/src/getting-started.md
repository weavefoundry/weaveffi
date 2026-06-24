# Getting Started

This guide walks you through installing WeaveFFI, writing an API as safe Rust,
generating multi-language bindings from it, and calling the result from C.

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

## 2) Create a producer crate

Create a library crate and add the `weaveffi` facade:

```bash
cargo new --lib my-math
cd my-math
cargo add weaveffi
```

Build a `cdylib` so the C ABI symbols are exported (keep `rlib` if you want to
unit-test the safe functions in-crate). In `Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

## 3) Write your API as safe Rust

Replace `src/lib.rs` with an annotated module that has a record and a function:

```rust
/// A tiny 2-D math module.
#[weaveffi::module]
pub mod math {
    /// A point in the plane.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Point {
        pub x: f64,
        pub y: f64,
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}

// Emit the fixed C ABI runtime surface once per cdylib.
weaveffi::export_runtime!();
```

`#[weaveffi::module]` reads the annotated items and generates the
`#[no_mangle] extern "C"` thunks that back the C ABI. A `Result<T, E>`-returning
function is fallible (the error flows through the ABI's `out_err` channel), a
`u64` is an opaque `handle`, and `String`/`Vec<u8>`/`Option<T>`/`Vec<T>` map to
`string`/`bytes`/optionals/lists. See
[The Rust Producer Macro](guides/producer-macro.md) for the full attribute and
type reference.

> **Prefer to design the contract first?** Run `weaveffi new my-project` to
> scaffold an IDL-based starter (`weaveffi.yml`) instead, then author the API in
> YAML/JSON/TOML. The [IDL Schema](reference/idl.md) reference covers that path;
> the rest of this guide assumes the macro.

## 4) Generate bindings

Point the generator straight at your source to produce bindings for all
targets:

```bash
weaveffi generate src/lib.rs -o generated
```

There is no scaffold step: the macro already generated the producer glue, and
`generate` reads the same annotations to emit the bindings. The output tree
looks like:

```text
generated/
├── c/          # C header + convenience stubs
├── swift/      # SwiftPM package + Swift wrapper
├── android/    # Kotlin JNI wrapper + Gradle skeleton
├── node/       # N-API loader + TypeScript types
└── wasm/       # WASM loader stub
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

## 6) Build the cdylib

The macro already emitted every `#[no_mangle] extern "C"` thunk, so there is
nothing to fill in by hand. Build the shared library:

```bash
cargo build
```

This produces `libmy_math.dylib` on macOS (`libmy_math.so` on Linux,
`my_math.dll` on Windows), exporting `weaveffi_math_add`, the
`weaveffi_math_Point_*` lifecycle and getters, and the runtime symbols from
`export_runtime!()`. The signatures match `generated/c/weaveffi.h` by
construction, because the thunks and the header are two views of one annotated
source. To report a recoverable failure from a function, return
`Result<T, E>`: the macro routes `Err` to the call's `out_err` parameter and
returns a zero or null sentinel.

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
- Read [The Rust Producer Macro](guides/producer-macro.md) for the full
  attribute set, the type mapping, cross-module references, and the feature
  roadmap.
- See the [Calculator tutorial](tutorials/calculator.md) for a full end-to-end
  walkthrough including Swift and Node.js.
- Read the [IDL Schema](reference/idl.md) reference for all supported types
  and features.
- Explore the [Generators](generators/README.md) section for target-specific
  details.

## Checking a single target

`weaveffi doctor` runs every toolchain check it knows about. To narrow it
down to a single target, pass `--target {name}`:

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
