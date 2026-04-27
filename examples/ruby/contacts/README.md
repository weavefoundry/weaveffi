# Ruby Contacts Example

A Ruby consumer example for the generated `samples/contacts` gem. It runs a
small CRUD demo and shows that returned `Contact` objects own native memory via
`FFI::AutoPointer`.

## Prerequisites

- Ruby >= 3.0
- Bundler
- Rust toolchain installed

## 1. Build the contacts cdylib

From the repo root:

```bash
cargo build -p contacts
```

The generated Ruby bindings load the library under the default `weaveffi` name,
so make the contacts cdylib available with that name:

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

## 2. Generate the Ruby gem

From the repo root:

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target ruby
```

The example `Gemfile` points Bundler at `../../../generated/ruby`.

## 3. Install Ruby dependencies

```bash
cd examples/ruby/contacts
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
=== Ruby Contacts Example ===

Created contact #1
Created contact #2

Total: 2 contacts

All contacts:
  [1] Alice Smith <alice@example.com> (Personal)
  [2] Bob Jones (Work)
Released list copies with Contact#destroy

Get contact #1:
  [1] Alice Smith <alice@example.com> (Personal)

AutoPointer cleanup:
  owned by FFI::AutoPointer: true
```
