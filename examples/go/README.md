# Go End-to-End Example

Loads the calculator and contacts cdylibs at runtime via
[`purego`](https://github.com/ebitengine/purego) and exercises a
representative slice of the C ABI: `add`, `create_contact`,
`list_contacts`, `delete_contact`. Prints `OK` and exits 0 on success.

## Prerequisites

- Go 1.21+

## Run

```bash
cargo build -p calculator -p contacts

cd examples/go
go mod download

WEAVEFFI_LIB=../../target/debug/libcalculator.dylib \
CONTACTS_LIB=../../target/debug/libcontacts.dylib \
  go run .
```

On Linux replace `.dylib` with `.so`. Or run via `examples/run_all.sh`.
