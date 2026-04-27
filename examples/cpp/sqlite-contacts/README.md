# SQLite Contacts C++ Example

A CMake project that consumes the generated C++ bindings for the
`samples/sqlite-contacts` sample — a SQLite-backed module that exercises
every production-shaped IDL feature at once.

It demonstrates:

- **Async** — every CRUD entry point is generated as
  `std::future<T>`. Awaiting is just `fut.get()`.
- **Cancellation driven by `std::future::wait_for(timeout)`** —
  `contacts_create_contact` is declared `cancellable: true`, so the
  generated wrapper takes an optional `std::stop_token`. The example
  launches the call, waits up to 20 ms, then flips a
  `std::stop_source`; the wrapper forwards this to the native cancel
  token and the Rust worker returns `ERR_CODE_CANCELLED`, which surfaces
  as a `weaveffi::WeaveFFIError` when we `.get()` the future.
- **Streaming iterator** — `list_contacts` returns `iter<Contact>`. The
  sqlite-contacts cdylib exposes the streaming
  `ListContactsIterator` + `_next` / `_destroy` ABI, which we iterate
  via a tiny helper in `iter_contacts.{hpp,cpp}` and wrap each yielded
  handle in a RAII `weaveffi::Contact`.

## Prerequisites

- CMake 3.16+
- A C++20 compiler (clang, gcc, or MSVC) — C++20 is required for
  `std::stop_source` / `std::stop_token`.
- A recent Rust toolchain

## 1. Build the sqlite-contacts cdylib

From the repo root:

```bash
cargo build -p sqlite-contacts
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libsqlite_contacts.dylib`
- Linux: `target/debug/libsqlite_contacts.so`
- Windows: `target\debug\sqlite_contacts.dll`

## 2. Regenerate the C++ bindings for the sqlite-contacts IDL

The checked-in `generated/cpp/` may track a different sample; regenerate
against `samples/sqlite-contacts/sqlite_contacts.yml` so `weaveffi.hpp`
exposes the contacts functions and `Contact`/`Status` types:

```bash
cargo run -p weaveffi-cli -- generate \
    samples/sqlite-contacts/sqlite_contacts.yml \
    -o generated --target cpp
```

## 3. Configure and build this example

### macOS / Linux

```bash
cd examples/cpp/sqlite-contacts
cmake -S . -B build
cmake --build build
```

### Windows (Visual Studio)

```powershell
cd examples\cpp\sqlite-contacts
cmake -S . -B build
cmake --build build --config Debug
```

By default, the CMake project looks for the cdylib in
`../../../target/debug`. Pass `-DSQLITE_CONTACTS_LIB_DIR=/absolute/path`
to override (for example when building `--release`):

```bash
cmake -S . -B build -DSQLITE_CONTACTS_LIB_DIR=$PWD/../../../target/release
```

## 4. Run it

### macOS

On macOS the CMake project already embeds a build rpath pointing at
`SQLITE_CONTACTS_LIB_DIR`, so you can just run the binary:

```bash
./build/sqlite_contacts
```

### Linux

```bash
LD_LIBRARY_PATH=../../../target/debug ./build/sqlite_contacts
```

### Windows

```powershell
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
.\build\Debug\sqlite_contacts.exe
```

Expected output:

```
=== C++ SQLite Contacts Example ===

Created #1 Alice
Created #2 Bob

Found #1: Alice <alice@example.com>
Updated alice's email: true

Iterating contacts:
  [1] Alice <alice@new.com> (Active)
  [2] Bob <-> (Active)

Total=2 Active=2

Cancelling a slow create via wait_for(20ms)...
  cancelled: code=2 message="cancelled"

Deleted bob: true
Remaining: 1
```

## Why the extra `iter_contacts.{hpp,cpp}`?

The C++ generator materialises `iter<T>` return types into a
`std::vector<T>`, so `weaveffi.hpp` declares
`weaveffi_contacts_list_contacts` with a list-style
`Contact** + size_t*` signature. The sqlite-contacts cdylib instead
implements the streaming iterator C ABI
(`ListContactsIterator*` + `_next` / `_destroy`), which is not
ABI-compatible with that signature. `iter_contacts.cpp` is a separate
translation unit that redeclares `weaveffi_contacts_list_contacts` with
its real signature and bridges the gap, returning raw handles that
`main.cpp` wraps in RAII `weaveffi::Contact` objects.
