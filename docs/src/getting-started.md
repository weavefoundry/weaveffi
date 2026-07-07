# Getting Started

This guide walks you through installing WeaveFFI, defining an API as a
language-neutral IDL, generating multi-language bindings from it, implementing
the native library behind the generated C ABI, and calling it from C.

WeaveFFI works with any native library that exposes a C ABI, so the producer
can be written in Rust, C, C++, Zig, or anything else that can speak C. This
guide implements it in Rust because that's the quickest to set up. If you're
writing a Rust producer, you can also let the `#[weaveffi::module]` macro
generate the C ABI and derive the IDL for you, instead of hand-writing YAML
(see step 2).

## Prerequisites

You need the [Rust toolchain](https://rustup.rs/) (stable channel) to install
the CLI, and for this guide's Rust producer. Verify with:

```bash
rustc --version
cargo --version
```

The CLI is the only hard requirement. The library you generate bindings for can
be written in any language that exposes a C ABI.

## 1) Install WeaveFFI

Install the CLI from crates.io:

```bash
cargo install weaveffi-cli
```

This puts the `weaveffi` binary on your `PATH`.

## 2) Define your API as an IDL

Describe the API once in a language-neutral IDL. Create `math.yml` with a
record and a function:

```yaml
version: "0.4.0"
package:
  name: my-math
  version: "0.1.0"
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

The optional `package:` block sets the name and version stamped into every
generated package manifest (`package.json`, `pyproject.toml`, `Package.swift`,
and so on). The IDL also supports primitives (`i32`, `f64`, `bool`, `string`,
`bytes`, `handle`), optionals (`string?`), and lists (`[i32]`). See the
[IDL Schema](reference/idl.md#package-metadata) reference for the full
specification.

> **Prefer not to hand-write YAML?** Run `weaveffi new my-project` to scaffold a
> starter project (an example IDL plus a `Cargo.toml` and `src/lib.rs` stub) you
> can edit instead.

> **Writing a Rust producer?** You can make annotated Rust the single source of
> truth instead of a separate IDL: annotate a module with `#[weaveffi::module]`
> and point the generator straight at the source. The macro emits the C ABI and
> derives the IDL from your code, so you write no `unsafe` glue. See
> [The Rust Producer Macro](guides/producer-macro.md). The rest of this guide
> uses the IDL.

## 3) Generate bindings

Run the generator to produce bindings for all targets:

```bash
weaveffi generate math.yml -o generated --scaffold
```

The `--scaffold` flag also emits a `scaffold.rs` with Rust FFI stubs you can
use as a starting point. The output tree looks like:

```text
generated/
├── c/          # C header + convenience stubs
├── swift/      # SwiftPM package + Swift wrapper
├── android/    # Kotlin JNI wrapper + Gradle skeleton
├── node/       # N-API loader + TypeScript types
├── wasm/       # Wasm loader stub
└── scaffold.rs # Rust FFI function stubs
```

## 4) Examine the generated output

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

## 5) Implement the library behind the C ABI

The generated C header (`generated/c/weaveffi.h`) is the contract your native
library must satisfy, and it's the same contract every language binding calls
into. You can implement it in any language that can expose a C ABI; here we use
Rust, starting from the generated `scaffold.rs`, which already contains a
`#[no_mangle] extern "C"` stub (with a `todo!()` body) for every symbol in the
header.

Create a library crate, add the WeaveFFI ABI helpers, and build a `cdylib`:

```bash
cargo new --lib my-math
cd my-math
cargo add weaveffi-abi
```

In `Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]
```

Copy `scaffold.rs` into `src/lib.rs` and fill in the bodies. Implementing `add`
looks like this (struct lifecycle omitted for brevity):

```rust
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

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

// Emit the fixed WeaveFFI C ABI runtime surface (free_string, free_bytes,
// error_clear, cancel_token_*) in one line. Call this exactly once per
// cdylib.
abi::export_runtime!();
```

Key points:

- Every exported function uses `#[no_mangle]` and `extern "C"`.
- `out_err` must always be cleared on success with `abi::error_set_ok`.
- On error, call `abi::error_set(out_err, code, message)` and return a
  zero/null value.
- The library must export the WeaveFFI runtime symbols: invoke
  [`weaveffi_abi::export_runtime!()`][export-runtime-doc] to emit all of
  them in one line instead of writing each `#[no_mangle]` thunk by hand.

[export-runtime-doc]: https://docs.rs/weaveffi-abi/latest/weaveffi_abi/macro.export_runtime.html

> **Tip for Rust producers:** the `#[weaveffi::module]` macro generates these
> `#[no_mangle] extern "C"` thunks for you from safe Rust, so you never fill in
> stubs by hand. See [The Rust Producer Macro](guides/producer-macro.md).

Build with:

```bash
cargo build
```

This produces a shared library (`libmy_math.dylib` on macOS,
`libmy_math.so` on Linux, `my_math.dll` on Windows). The exported symbols match
`generated/c/weaveffi.h` by construction.

## 6) Build and test with C

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
- Read the [IDL Schema](reference/idl.md) reference for all supported types
  and features.
- Writing a Rust producer? See
  [The Rust Producer Macro](guides/producer-macro.md) to skip the scaffold and
  generate the C ABI directly from annotated Rust.
- See the [Calculator tutorial](tutorials/calculator.md) for a full end-to-end
  walkthrough including Swift and Node.js.
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
