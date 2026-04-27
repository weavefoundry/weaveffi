# Contacts C++ Example

A CMake project that consumes the generated C++ RAII wrappers for the
`samples/contacts` sample.

It exercises:

- `weaveffi::contacts_create_contact` / `contacts_get_contact` /
  `contacts_list_contacts` / `contacts_delete_contact` /
  `contacts_count_contacts`.
- The `weaveffi::Contact` move-only RAII type, which calls
  `weaveffi_contacts_Contact_destroy` from its destructor.
- The generated `weaveffi::WeaveFFIError` exception type.

## Prerequisites

- CMake 3.16+
- A C++17 compiler (clang, gcc, or MSVC)
- A recent Rust toolchain

## 1. Build the contacts cdylib

From the repo root:

```bash
cargo build -p contacts
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libcontacts.dylib`
- Linux: `target/debug/libcontacts.so`
- Windows: `target\debug\contacts.dll`

## 2. Regenerate the C++ bindings for the contacts IDL

The checked-in `generated/cpp/` tracks the calculator sample by default; regenerate
against `samples/contacts/contacts.yml` so `weaveffi.hpp` exposes the contacts
functions:

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target cpp
```

## 3. Configure and build this example

### macOS / Linux

```bash
cd examples/cpp/contacts
cmake -S . -B build
cmake --build build
```

### Windows (Visual Studio)

```powershell
cd examples\cpp\contacts
cmake -S . -B build
cmake --build build --config Debug
```

By default, the CMake project looks for the cdylib in
`../../../target/debug`. Pass `-DCONTACTS_LIB_DIR=/absolute/path` to override
(for example when building `--release`):

```bash
cmake -S . -B build -DCONTACTS_LIB_DIR=$PWD/../../../target/release
```

## 4. Run it

The generated `weaveffi_cpp` CMake target is an INTERFACE library, so the
contacts cdylib is loaded dynamically at runtime. Set the appropriate library
search path to point at `target/debug/` (or wherever you built the cdylib):

### macOS

On macOS the CMake project already embeds a build rpath pointing at
`CONTACTS_LIB_DIR`, so you can just run the binary:

```bash
./build/contacts
```

If you moved the cdylib elsewhere, either set `DYLD_LIBRARY_PATH` or rebuild:

```bash
DYLD_LIBRARY_PATH=../../../target/debug ./build/contacts
```

### Linux

```bash
LD_LIBRARY_PATH=../../../target/debug ./build/contacts
```

### Windows

Add the directory containing `contacts.dll` to `PATH` before running:

```powershell
$env:PATH = "$PWD\..\..\..\target\debug;$env:PATH"
.\build\Debug\contacts.exe
```

Expected output:

```
=== C++ Contacts Example ===

Created contact #1
Created contact #2

Total: 2 contacts

  [1] Alice Smith <alice@example.com> (Personal)
  [2] Bob Jones (Work)

Fetched: Alice Smith
Deleted contact #2: true
Remaining: 1 contact(s)
```
