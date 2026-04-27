# C++ End-to-End Example

Loads the calculator and contacts cdylibs at runtime via `dlopen` and
exercises a representative slice of the C ABI: `add`, `create_contact`,
`list_contacts`, `delete_contact`. Prints `OK` and exits 0 on success.

## Prerequisites

- C++17 compiler (`g++` or `clang++`)
- CMake >= 3.14

## Run

```bash
cargo build -p calculator -p contacts

cmake -S examples/cpp -B target/cpp-example
cmake --build target/cpp-example

WEAVEFFI_LIB=target/debug/libcalculator.dylib \
CONTACTS_LIB=target/debug/libcontacts.dylib \
  ./target/cpp-example/cpp_example
```

On Linux replace `.dylib` with `.so`. Or run via `examples/run_all.sh`.
