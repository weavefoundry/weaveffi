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
version: "0.9.0"
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

The IDL describes only the API. It supports primitives (`i32`, `f64`, `bool`,
`string`, `bytes`, and the rest of the fixed-width integers and floats),
optionals (`string?`), lists (`[i32]`), maps (`{string:i64}`), lazy iterators
(`iter<string>`), records and rich enums, interfaces (reference-counted
objects with constructors, methods, and statics), callback interfaces
(methods the consumer implements and the native library calls), async
functions, and typed error domains (opt in per function with `throws: true`).
See the [IDL Schema](reference/idl.md) reference for the full specification
and the [C ABI Contract](reference/abi.md) for how each of them crosses the
boundary.

Everything about how the API is *published* lives in an optional
`weaveffi.toml` next to it. Create one so the generated package manifests
(`package.json`, `pyproject.toml`, `Package.swift`, and so on) carry your name
and version:

```toml
[package]
name = "my-math"
version = "0.1.0"
```

The CLI picks up the nearest `weaveffi.toml` at or above the input file
automatically; pass `--config` to point at a different one. The same file
holds per-target options under `[generators.<target>]` tables; see
[Configuration](guides/config.md).

> **Writing a Rust producer?** You can make annotated Rust the single source of
> truth instead of a separate IDL: annotate a module with `#[weaveffi::module]`
> and point the generator straight at the source. The macro emits the C ABI and
> derives the IDL from your code, so you write no `unsafe` glue. See
> [The Rust Producer Macro](guides/producer-macro.md). The rest of this guide
> uses the IDL.

## 3) Generate bindings

Run the generator to produce bindings for all targets:

```bash
weaveffi generate math.yml -o generated
```

Pass `--target c,swift,node` to generate a subset. The output tree has one
directory per target:

```text
generated/
├── c/          # C header + convenience stubs
├── cpp/        # RAII C++ header + CMakeLists.txt
├── swift/      # SwiftPM package + Swift wrapper
├── kotlin/     # Kotlin JNI wrapper + Gradle (build.gradle.kts) project
├── node/       # N-API addon + TypeScript types
├── wasm/       # JavaScript loader + TypeScript types
├── python/     # ctypes bindings + .pyi stubs
├── dotnet/     # C# P/Invoke bindings
├── dart/       # dart:ffi bindings
├── go/         # cgo bindings
└── ruby/       # FFI gem bindings
```

## 4) Examine the generated output

### C header (`generated/c/weaveffi.h`)

Records generate no C functions: a `Point` crosses the ABI serialized as
a [value buffer](reference/value-buffers.md), a single
`(const uint8_t*, size_t)` pair, and the header opens with a comment
block spelling out that convention. What remains is one prototype per
module-level function, each taking an `out_err` parameter for error
reporting:

```c
/*
 * Value buffer convention: records, rich enums, lists, maps, and
 * optionals cross the ABI serialized in the WeaveFFI value buffer
 * format ...
 */

// Module: math
WEAVEFFI_API int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);
```

The header also declares the fixed runtime surface every producer exports
(`weaveffi_abi_version`, the `weaveffi_error` struct and its helpers,
`weaveffi_free_string`/`weaveffi_free_bytes`, and the cancel-token family);
see the [C ABI Contract](reference/abi.md). Had `math.yml` declared an
interface, the header would also carry an opaque typedef plus `_clone` and
`_destroy` symbols for it, and a callback interface would appear as a vtable
typedef.

### Swift wrapper (`generated/swift/Sources/MyMath/MyMath.swift`)

Structs become plain Swift structs with typed properties, packed and
unpacked from value buffers by the wrapper. Module functions are grouped
under a Swift enum namespace. Because `add` doesn't declare
`throws: true`, its Swift wrapper is a plain non-throwing function:

```swift
public struct Point {
    public var x: Double
    public var y: Double

    public init(x: Double, y: Double) { ... }
}

public enum Math {
    public static func add(a: Int32, b: Int32) -> Int32 { ... }
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
Rust and write the one `#[no_mangle] extern "C"` function the header declares
by hand.

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

Implementing `add` in `src/lib.rs` looks like this:

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

// Emit the fixed WeaveFFI C ABI runtime surface (abi_version, error_set,
// error_clear, error_free, free_string, free_bytes, cancel_token_*) in one
// line. Call this exactly once per cdylib.
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
  Among them is `weaveffi_abi_version()`, which reports ABI revision 2 and
  which the generated Python, Ruby, Dart, Go, .NET, Node.js, and Wasm
  bindings call at load time to refuse a library built against a different
  revision.
- An interface in your IDL adds a `_clone` and `_destroy` pair you implement
  with `weaveffi_abi::object_clone` and `object_destroy` over an `Arc<T>`;
  the [C ABI Contract](reference/abi.md#objects-interfaces) spells out the
  reference-counting rules. The `#[weaveffi::module]` macro writes all of
  this for you.

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

- Read the [IDL Schema](reference/idl.md) reference for all supported types
  and features, and the [C ABI Contract](reference/abi.md) for how objects,
  callback interfaces, value buffers, async functions, and iterators cross
  the boundary.
- Writing a Rust producer? See
  [The Rust Producer Macro](guides/producer-macro.md) to generate the C ABI
  directly from annotated Rust instead of implementing the header by hand.
- Look at the [samples](samples.md): `kvstore` exercises every IDL feature,
  and `events` is the smallest example of a reference-counted object plus a
  callback interface.
- See the [Calculator tutorial](tutorials/calculator.md) for a full end-to-end
  walkthrough including Swift and Node.js.
- Explore the [Generators](generators/README.md) section for target-specific
  details, and [Configuration](guides/config.md) for `weaveffi.toml`.
- Add `weaveffi diff --check` to CI so regenerated bindings can't drift from
  the committed ones; see [Stability and Versioning](stability.md).
