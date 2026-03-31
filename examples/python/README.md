# Python Contacts Example

## Prerequisites

- Python >= 3.8
- Rust toolchain installed

## Steps

1. Generate Python bindings (from repo root):

```bash
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target python
```

2. Build the contacts cdylib:

```bash
cargo build -p contacts
```

3. Copy the shared library into the generated Python package directory:

```bash
# macOS
cp target/debug/libcontacts.dylib generated/python/weaveffi/libweaveffi.dylib

# Linux
cp target/debug/libcontacts.so generated/python/weaveffi/libweaveffi.so
```

4. Run the example:

```bash
python examples/python/contacts.py
```
