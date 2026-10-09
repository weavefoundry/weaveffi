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

Create a library crate and add the `weaveffi` crate. It needs no
`crate-type`: WeaveFFI builds the C dynamic library itself (with `cargo
rustc --crate-type cdylib`).

```bash
cargo new --lib mathlib
cd mathlib
cargo add weaveffi
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
        /// division by zero
        DivisionByZero = 1,
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
- `Result<i32, MathError>` makes `div` throw the `MathError` domain. The
  enum's discriminants are the stable error codes, and the macro generates
  its `Display` from each variant's doc comment (or a
  `#[weaveffi(message = "...")]` template), which is the message consumers
  see. A `Result` whose error type isn't a declared domain (a `String`, a
  `std::io::Error`) works too: it throws an untyped error carrying its
  `Display` text.
- `weaveffi::export_runtime!()` appears exactly once, at the crate root.

The [producer macro guide](guides/producer-macro.md) covers records, enums,
objects, callbacks, iterators, and async functions.

## Configure and generate

Run `weaveffi init` in the crate. It writes a `weaveffi.toml` whose
`[project]` table points at the crate itself, then lists anything the crate
still needs (the `weaveffi` dependency, the `export_runtime!` call, a
`#[weaveffi::module]`):

```bash
weaveffi init
```

The file it writes generates the C header and the Python package; edit
`targets` to pick the languages you ship (without it, every target is
generated):

```toml
# weaveffi.toml
[project]
input = "."                 # this crate
out = "bindings"
targets = ["c", "python"]
```

Generate the bindings. With a `[project]` table, `weaveffi generate` needs
no arguments and works from any directory in the project:

```bash
weaveffi generate
```

`weaveffi generate` builds the library with Cargo's `dev` profile (pass
`--profile <name>` for another), then reads the API from the library it
built: the macro embeds a description of every exported declaration in
the library, so the bindings match the compiled code exactly, `#[cfg]`
included. `weaveffi extract` prints that description as an IDL.

The output has one directory per target. The C target writes
`bindings/c/mathlib.h`; the Python target writes an installable package
named `mathlib`. Run `weaveffi generate` again after any change: it rewrites
only the files whose contents changed and removes files a previous run wrote
that are no longer produced.

## Call it from Python

Run `weaveffi dev` instead of `weaveffi generate` while you iterate: it
builds the library (the `dev` profile), generates, and copies the library into the
generated Python package, which loads a bundled copy first. Then install the
package:

```bash
weaveffi dev
pip install ./bindings/python
```

To load a library from somewhere else, set `MATHLIB_LIBRARY` to its full
path; the Python, Ruby, .NET, Dart, and Kotlin (JVM) packages all honor that
`{PREFIX}_LIBRARY` variable at run time, and `weaveffi dev` prints the value
for them. A packaged release bundles the library instead (see
[Packaging](guides/packaging.md)).

```python
import mathlib

print(mathlib.add(2, 3))          # 5
print(mathlib.greet("Python"))    # Hello, Python!

try:
    mathlib.div(1, 0)
except mathlib.MathError as e:
    print(e.code, e.message)      # 1 division by zero
```

On import the package checks the library's ABI revision and the `math`
module's contract table, so a library built from incompatible source fails
to load with an error naming the declaration that's missing or changed. The
[Python page](generators/python.md) documents the generated surface.

## Call it from C

The header is the contract every other binding is built on. Strings cross as
UTF-8 `(ptr, len)` runs, the caller owns a zeroed `mathlib_error` (whose
message is a length-delimited run, not a C string), and returned strings are
released with `mathlib_free_bytes`:

```c
#include <stdio.h>
#include <string.h>
#include "mathlib.h"

int main(void) {
    if (mathlib_abi_version() != MATHLIB_ABI_VERSION ||
        mathlib_math_contract_check() != 0) {
        fprintf(stderr, "mathlib.h does not match the loaded library\n");
        return 1;
    }

    mathlib_error err = {0};
    printf("2 + 3 = %d\n", mathlib_math_add(2, 3, &err));

    mathlib_math_div(1, 0, &err);
    if (err.code == mathlib_math_MathError_DivisionByZero) {
        printf("error %d: %.*s\n", err.code,
               (int)err.message_len, (const char*)err.message_ptr);
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

## Ship it

```bash
weaveffi package --target python,node
```

`weaveffi package` builds the library in release mode for this machine
(that's `weaveffi build`), then writes installable artifacts to `dist/`:
here a wheel with the library inside, and npm tarballs carrying the library
and a prebuilt addon. Neither needs `MATHLIB_LIBRARY` or a compiler:

```bash
pip install dist/python/mathlib-*.whl
npm install dist/node/*.tgz
```

[Packaging](guides/packaging.md) covers more platforms (iOS, Android,
Windows, `wasm32`), every target's artifacts, and publishing.

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
version: "0.12.0"
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
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include "greeter.h"

uint32_t greeter_abi_version(void) { return GREETER_ABI_VERSION; }

/* The contract table the header was generated with, sorted by id. */
const greeter_contract_entry* greeter_greeter_contract(size_t* out_len) {
    static const greeter_contract_entry table[] = GREETER_GREETER_CONTRACT;
    if (out_len != NULL) *out_len = GREETER_GREETER_CONTRACT_LEN;
    return table;
}

int32_t greeter_greeter_add(int32_t a, int32_t b, greeter_error* out_err) {
    (void)out_err;   /* written only on failure */
    return (int32_t)((uint32_t)a + (uint32_t)b);
}

/* One allocator for every byte run: returned strings and buffers, error
   messages and payloads, and runs a consumer allocates with greeter_alloc.
   Every run must be 8-aligned; malloc's alignment is at least 8. */
uint8_t* greeter_alloc(size_t len) { return len ? calloc(len, 1) : NULL; }
void greeter_free_bytes(uint8_t* ptr, size_t len) { if (len) free(ptr); }

static const uint8_t* run_copy(const uint8_t* ptr, size_t len) {
    uint8_t* run = greeter_alloc(len);
    if (run != NULL) memcpy(run, ptr, len);
    return run;
}

/* The message is `message_len` bytes of UTF-8, not NUL-terminated; NULL is
   the empty message. Both setters copy. */
void greeter_error_set(greeter_error* err, int32_t code,
                       const uint8_t* message_ptr, size_t message_len) {
    if (err == NULL) return;
    greeter_error_clear(err);
    err->code = code;
    if (message_ptr != NULL && message_len != 0) {
        err->message_ptr = run_copy(message_ptr, message_len);
        err->message_len = err->message_ptr != NULL ? message_len : 0;
    }
}

void greeter_error_set_payload(greeter_error* err, const uint8_t* ptr, size_t len) {
    if (err == NULL) return;
    free((void*)err->payload_ptr);
    err->payload_ptr = NULL;
    err->payload_len = 0;
    if (ptr != NULL && len != 0) {
        err->payload_ptr = run_copy(ptr, len);
        err->payload_len = err->payload_ptr != NULL ? len : 0;
    }
}

void greeter_error_clear(greeter_error* err) {
    if (err == NULL) return;
    free((void*)err->message_ptr);
    free((void*)err->payload_ptr);
    memset(err, 0, sizeof *err);
}

void greeter_error_free(greeter_error* err) {
    greeter_error_clear(err);
    free(err);
}

/* Cancel tokens (used by async functions; this API has none, but the runtime
   surface is the same for every library). */
struct greeter_cancel_token { atomic_int refs; atomic_bool cancelled; };
greeter_cancel_token* greeter_cancel_token_create(void) {
    greeter_cancel_token* t = malloc(sizeof *t);
    if (t != NULL) { atomic_init(&t->refs, 1); atomic_init(&t->cancelled, false); }
    return t;
}
void greeter_cancel_token_cancel(greeter_cancel_token* token) {
    if (token != NULL) atomic_store(&token->cancelled, true);
}
bool greeter_cancel_token_is_cancelled(const greeter_cancel_token* token) {
    return token != NULL && atomic_load(&((greeter_cancel_token*)token)->cancelled);
}
void greeter_cancel_token_destroy(greeter_cancel_token* token) {
    if (token != NULL && atomic_fetch_sub(&token->refs, 1) == 1) free(token);
}

/* Leak counters: a producer that doesn't count returns 0 for every kind. */
uint64_t greeter_debug_live(int32_t kind) { (void)kind; return 0; }
```

The library must export every runtime symbol the header declares, each
top-level module's contract function, and the API itself. Every run it hands
out is 8-aligned and released by its own `greeter_free_bytes`, and a
callback vtable it's given may carry the `GREETER_VTABLE_THREAD_AFFINE`
flag, which it must honor. The
[C ABI contract](reference/abi.md#runtime-surface) lists them, and
[`conformance/c/producer.c`](https://github.com/weavefoundry/weaveffi/blob/main/conformance/c/producer.c)
is a complete hand-written producer to copy from. Build it as
`libgreeter.so` (`libgreeter.dylib` on macOS, `greeter.dll` on Windows):

```bash
cc -shared -fPIC -I bindings/c greeter.c -o libgreeter.so
```

Then run `weaveffi generate` for the other targets; they load it exactly as
they load a Rust producer. From Python, for example:

```bash
weaveffi generate --target python
pip install ./bindings/python
GREETER_LIBRARY=$PWD/libgreeter.so python3 -c "import greeter; print(greeter.add(2, 3))"   # 5
```

## Next steps

- [Project Configuration](guides/config.md): `[package]` metadata, `[build]`
  settings, per-target options, and how regeneration cleans up stale files.
- [Packaging](guides/packaging.md): building every platform and publishing
  the artifacts.
- [Samples](samples.md): three complete producers, from the minimal
  `calculator` to the feature-complete `kvstore`.
- [Generators](generators/README.md): what each language gets.
- Gate CI on `weaveffi generate --check` so committed bindings can't drift; see
  [Stability and Versioning](stability.md#ci-workflow).
