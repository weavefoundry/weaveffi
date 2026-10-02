# Getting Started

This guide builds a small Rust library, generates bindings for it, and calls
it from Python and from C. A second, shorter path at the end shows how to
start from an IDL instead and implement the generated C header in C.

## Install the CLI

You need a stable [Rust toolchain](https://rustup.rs/). Install the
`weaveffi` CLI from crates.io (or grab a prebuilt binary with
`cargo binstall weaveffi-cli`):

```bash
cargo install weaveffi-cli
weaveffi --version
```

## Write the producer

Create a library crate, add the `weaveffi` crate, and build it as a C
dynamic library:

```bash
cargo new --lib mathlib
cd mathlib
cargo add weaveffi
```

```toml
# Cargo.toml
[lib]
crate-type = ["cdylib"]
```

Replace `src/lib.rs` with an annotated module. Everything here is safe Rust;
the macro writes the `extern "C"` layer.

```rust
/// Integer arithmetic and greetings.
#[weaveffi::module]
pub mod math {
    /// Errors the math functions report.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum MathError {
        /// Division by zero.
        DivisionByZero = 1,
    }

    impl std::fmt::Display for MathError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("division by zero")
        }
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Divide `a` by `b`, failing when `b` is zero.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, MathError> {
        a.checked_div(b).ok_or(MathError::DivisionByZero)
    }

    /// Greet someone by name.
    #[weaveffi::export]
    pub fn greet(name: &str) -> String {
        format!("Hello, {name}!")
    }
}

// Export the runtime symbols (errors, memory, cancel tokens) once per library.
weaveffi::export_runtime!();
```

A few things to notice:

- Every C symbol starts with the crate's library name, so this crate exports
  `mathlib_math_add`, `mathlib_math_div`, and `mathlib_math_greet`, plus the
  runtime (`mathlib_error_clear`, `mathlib_free_bytes`, and so on).
- `Result<i32, MathError>` makes `div` a throwing function. The enum's
  discriminants are the stable error codes, and its `Display` output is the
  message consumers see.
- `weaveffi::export_runtime!()` appears exactly once, at the crate root.

The [producer macro guide](guides/producer-macro.md) covers records, enums,
objects, callbacks, iterators, and async functions.

## Configure and generate

Run `weaveffi init` in the crate. It writes a `weaveffi.toml` whose
`[project]` table points at `src/lib.rs`, then lists anything the crate still
needs (a `cdylib` crate type, the `weaveffi` dependency, the
`export_runtime!` call):

```bash
weaveffi init
```

```toml
# weaveffi.toml
[project]
input = "src/lib.rs"
out = "bindings"
targets = ["c", "python"]   # omit to generate all eleven
```

Build the library and generate the bindings. With a `[project]` table,
`weaveffi generate` needs no arguments and works from any directory in the
project:

```bash
cargo build
weaveffi generate
```

The output has one directory per target. The C target writes
`bindings/c/mathlib.h`; the Python target writes an installable package
named `mathlib`. Run `weaveffi generate` again after any change: it rewrites
only the files whose contents changed and removes files a previous run wrote
that are no longer produced.

## Call it from Python

Install the generated package and point it at the library you built. Every
generated loader honors a `{PREFIX}_LIBRARY` environment variable, here
`MATHLIB_LIBRARY`; a packaged release bundles the library instead (see
[Packaging](guides/packaging.md)).

```bash
pip install ./bindings/python
export MATHLIB_LIBRARY="$PWD/target/debug/libmathlib.dylib"   # .so on Linux
```

```python
import mathlib

print(mathlib.add(2, 3))          # 5
print(mathlib.greet("Python"))    # Hello, Python!

try:
    mathlib.div(1, 0)
except mathlib.MathError as e:
    print("caught:", e)           # caught: division by zero
```

On import the package checks the library's ABI revision and the contract
checksum of the `math` module, so a library built from different source
fails to load with an error naming the module. The
[Python page](generators/python.md) documents the generated surface.

## Call it from C

The header is the contract every other binding is built on. Strings cross as
UTF-8 `(ptr, len)` runs, the caller owns a zeroed `mathlib_error`, and
returned strings are released with `mathlib_free_bytes`:

```c
#include <stdio.h>
#include <string.h>
#include "mathlib.h"

int main(void) {
    if (mathlib_abi_version() != MATHLIB_ABI_VERSION ||
        mathlib_math_checksum() != MATHLIB_MATH_CHECKSUM) {
        fprintf(stderr, "mathlib.h does not match the loaded library\n");
        return 1;
    }

    mathlib_error err = {0};
    printf("2 + 3 = %d\n", mathlib_math_add(2, 3, &err));

    mathlib_math_div(1, 0, &err);
    if (err.code == mathlib_math_MathError_DivisionByZero) {
        printf("error %d: %s\n", err.code, err.message);
        mathlib_error_clear(&err);
    }

    const char* name = "C";
    size_t len = 0;
    const uint8_t* text =
        mathlib_math_greet((const uint8_t*)name, strlen(name), &len, &err);
    printf("%.*s\n", (int)len, (const char*)text);
    mathlib_free_bytes((uint8_t*)text, len);
    return 0;
}
```

```bash
cc -I bindings/c main.c -L target/debug -lmathlib -o main
DYLD_LIBRARY_PATH=target/debug ./main    # LD_LIBRARY_PATH on Linux
```

[Errors and Memory](guides/errors-and-memory.md) states every ownership rule
the generated bindings follow for you.

## Implementing an IDL in C

A producer doesn't have to be Rust. Start from an IDL, generate the C header,
and implement it in any language that can export C symbols. Outside a Rust
crate, `weaveffi init` writes a starter IDL and a `weaveffi.toml`:

```bash
mkdir greeter && cd greeter
weaveffi init           # writes greeter.yml and weaveffi.toml
```

Edit `greeter.yml` to declare the API (the [IDL reference](reference/idl.md)
has the full schema):

```yaml
version: "0.10.0"
modules:
  - name: greeter
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
```

For an IDL input the identity comes from `[package]` in `weaveffi.toml`
(`name = "greeter"`), so the prefix and the library are both `greeter`.
Generate the header and implement it:

```bash
weaveffi generate --target c     # writes bindings/c/greeter.h
```

```c
/* greeter.c */
#include <stdlib.h>
#include <string.h>
#include "greeter.h"

uint32_t greeter_abi_version(void) { return GREETER_ABI_VERSION; }
uint64_t greeter_greeter_checksum(void) { return GREETER_GREETER_CHECKSUM; }

int32_t greeter_greeter_add(int32_t a, int32_t b, greeter_error* out_err) {
    (void)out_err;   /* written only on failure */
    return a + b;
}

void greeter_error_set(greeter_error* err, int32_t code, const char* message) {
    greeter_error_clear(err);
    err->code = code;
    err->message = message ? strdup(message) : NULL;
}

void greeter_error_clear(greeter_error* err) {
    free((void*)err->message);
    free((void*)err->payload_ptr);
    memset(err, 0, sizeof *err);
}

void greeter_error_free(greeter_error* err) {
    if (err) { greeter_error_clear(err); free(err); }
}

void greeter_free_bytes(uint8_t* ptr, size_t len) { (void)len; free(ptr); }

/* Also required: the four greeter_cancel_token_* functions and
   greeter_debug_live (which may return 0). */
```

The library must export every runtime symbol the header declares, each
top-level module's checksum function, and the API itself; the
[C ABI contract](reference/abi.md#runtime-surface) lists them, and
[`conformance/c/producer.c`](https://github.com/weavefoundry/weaveffi/blob/main/conformance/c/producer.c)
is a complete hand-written producer to copy from. Build it as
`libgreeter.so` (`libgreeter.dylib`, `greeter.dll`), then run
`weaveffi generate` for the other targets; they load it exactly as they load
a Rust producer.

## Next steps

- [Project Configuration](guides/config.md): `[package]` metadata, per-target
  options, and the generation cache.
- [Samples](samples.md): six complete producers, from `calculator` to the
  kitchen-sink `kvstore`.
- [Generators](generators/README.md): what each language gets.
- Gate CI on `weaveffi diff --check` so committed bindings can't drift; see
  [Stability and Versioning](stability.md#ci-workflow).
