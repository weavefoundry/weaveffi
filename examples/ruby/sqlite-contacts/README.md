# Ruby SQLite Contacts Example

A Ruby consumer example for the generated `samples/sqlite-contacts` gem.

It demonstrates:

- Block-based async callbacks, for example
  `create_contact_async(name, email) { |result, err| ... }`
- Iterator consumption through the generated `Enumerator` returned by
  `list_contacts(nil)`
- Deterministic native handle cleanup with `Contact#destroy`

## Prerequisites

- Ruby >= 3.0
- Bundler
- Rust toolchain installed

## 1. Build the sqlite-contacts cdylib

From the repo root:

```bash
cargo build -p sqlite-contacts
```

The generated Ruby bindings load the library under the default `weaveffi` name,
so make the SQLite contacts cdylib available with that name:

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

## 2. Generate the Ruby gem

From the repo root:

```bash
cargo run -p weaveffi-cli -- generate samples/sqlite-contacts/sqlite_contacts.yml -o generated --target ruby
```

The example `Gemfile` points Bundler at `../../../generated/ruby`.

## 3. Install Ruby dependencies

```bash
cd examples/ruby/sqlite-contacts
bundle install
```

## 4. Run it

### Linux

```bash
LD_LIBRARY_PATH="$PWD/../../../target/debug" bundle exec ruby bin/contacts.rb
```

### macOS

```bash
DYLD_LIBRARY_PATH="$PWD/../../../target/debug" bundle exec ruby bin/contacts.rb
```

Expected output includes:

```text
=== Ruby SQLite Contacts Example ===

Created #1 Alice
Created #2 Bob
Updated Alice's email: true

Found #1 Alice <alice@new.com> (Active, created 2026-04-27T00:00:00Z)

Totals: all=2 active=2

All contacts from Enumerator (Enumerator):
  #1 Alice <alice@new.com> (Active, created 2026-04-27T00:00:00Z)
  #2 Bob <no email> (Active, created 2026-04-27T00:00:00Z)

Deleted Bob: true
Remaining: 1
```
