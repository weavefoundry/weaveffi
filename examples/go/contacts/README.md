# Contacts Go Example

A small Go program that consumes the generated CGo bindings for
`samples/contacts`.

It exercises:

- `ContactsCreateContact` / `ContactsCountContacts`
- `ContactsListContacts` — returns `[]*Contact`, where each element owns a
  native handle
- `ContactsGetContact` — returns one `*Contact`
- `ContactsDeleteContact`
- Explicit `Close()` calls on every `*Contact` returned by the generated
  bindings

## Prerequisites

- Go >= 1.21 with CGo enabled
- A C compiler available to CGo
- A recent Rust toolchain

## 1. Build the contacts cdylib

From the repo root:

```bash
cargo build -p contacts
```

The generated Go package links with `-lweaveffi`, so make the contacts cdylib
available under the default `weaveffi` name:

### macOS

```bash
ln -sf "$PWD/target/debug/libcontacts.dylib" \
       "$PWD/target/debug/libweaveffi.dylib"
```

### Linux

```bash
ln -sf "$PWD/target/debug/libcontacts.so" \
       "$PWD/target/debug/libweaveffi.so"
```

### Windows (PowerShell, developer command prompt)

```powershell
Copy-Item target\debug\contacts.dll target\debug\weaveffi.dll
```

## 2. Regenerate the C and Go bindings

This example's `go.mod` uses:

```text
replace github.com/example/weaveffi => ../../generated/go
```

From `examples/go/contacts`, that resolves to `examples/generated/go`, so
generate both the C header and Go module there:

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o examples/generated --target c
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o examples/generated --target go
```

## 3. Point CGo at the generated header and cdylib

From the repo root:

### macOS

```bash
export ROOT="$PWD"
export CGO_CFLAGS="-I$ROOT/examples/generated/c"
export CGO_LDFLAGS="-L$ROOT/target/debug -lweaveffi"
export DYLD_LIBRARY_PATH="$ROOT/target/debug"
```

### Linux

```bash
export ROOT="$PWD"
export CGO_CFLAGS="-I$ROOT/examples/generated/c"
export CGO_LDFLAGS="-L$ROOT/target/debug -lweaveffi"
export LD_LIBRARY_PATH="$ROOT/target/debug"
```

### Windows (PowerShell, developer command prompt)

```powershell
$env:ROOT = (Get-Location).Path
$env:CGO_CFLAGS = "-I$env:ROOT\examples\generated\c"
$env:CGO_LDFLAGS = "-L$env:ROOT\target\debug -lweaveffi"
$env:PATH = "$env:ROOT\target\debug;$env:PATH"
```

## 4. Run it

```bash
cd examples/go/contacts
go run .
```

Expected output:

```text
=== Go Contacts Example ===

Created contact #1
Created contact #2

Total: 2 contacts

All contacts:
  [1] Alice Smith <alice@example.com> (Personal)
  [2] Bob Jones (Work)

Get contact #1:
  [1] Alice Smith <alice@example.com> (Personal)

Deleted contact #2: true
Total: 1 contacts
```
