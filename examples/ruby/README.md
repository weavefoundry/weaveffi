# Ruby End-to-End Example

Loads the calculator and contacts cdylibs at runtime via the
[`ffi`](https://github.com/ffi/ffi) gem and exercises a representative
slice of the C ABI: `add`, `create_contact`, `list_contacts`,
`delete_contact`. Prints `OK` and exits 0 on success.

## Prerequisites

- Ruby 2.7+
- `ffi` gem (install with `gem install ffi` or `bundle install`)

## Run

```bash
cargo build -p calculator -p contacts

WEAVEFFI_LIB=target/debug/libcalculator.dylib \
CONTACTS_LIB=target/debug/libcontacts.dylib \
  ruby examples/ruby/main.rb
```

On Linux replace `.dylib` with `.so`. Or run via `examples/run_all.sh`.
