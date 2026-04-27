# SQLite Contacts Go Example

A small Go program that consumes the generated CGo bindings for
`samples/sqlite-contacts`.

It demonstrates:

- Channel-based async calls, for example
  `<-weaveffi.ContactsCreateContact(ctx, ...)`
- Iterator consumption via a receive-only Go channel:
  `for contact := range list_contacts() { ... }`
- Explicit `Close()` calls on every generated `*Contact` handle

## Prerequisites

- Go >= 1.21 with CGo enabled
- A C compiler available to CGo
- A recent Rust toolchain

## 1. Build the sqlite-contacts cdylib

From the repo root:

```bash
cargo build -p sqlite-contacts
```

The generated Go package links with `-lweaveffi`, so make the SQLite contacts
cdylib available under the default `weaveffi` name:

### macOS

```bash
ln -sf "$PWD/target/debug/libsqlite_contacts.dylib" \
       "$PWD/target/debug/libweaveffi.dylib"
```

### Linux

```bash
ln -sf "$PWD/target/debug/libsqlite_contacts.so" \
       "$PWD/target/debug/libweaveffi.so"
```

### Windows (PowerShell, developer command prompt)

```powershell
Copy-Item target\debug\sqlite_contacts.dll target\debug\weaveffi.dll
```

## 2. Regenerate the C and Go bindings

This example's `go.mod` uses:

```text
replace github.com/example/weaveffi => ../../generated/go
```

From `examples/go/sqlite-contacts`, that resolves to `examples/generated/go`,
so generate both the C header and Go module there:

```bash
cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o examples/generated --target c
cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o examples/generated --target go
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
cd examples/go/sqlite-contacts
go run .
```

Expected output:

```text
=== Go SQLite Contacts Example ===

Created #1 Alice
Created #2 Bob
Updated Alice's email: true

Found #1 Alice <alice@new.com> (Active, created 2026-04-27T00:00:00Z)

Total before delete: 2

All contacts from iterator channel:
  #1 Alice <alice@new.com> (Active, created 2026-04-27T00:00:00Z)
  #2 Bob <no email> (Active, created 2026-04-27T00:00:00Z)

Deleted Bob: true
Remaining: 1
```
