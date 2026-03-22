# Node Examples (N-API addon)

## Prerequisites

- Node.js 22+ installed
- Rust toolchain installed

## Calculator (`main.mjs`)

1. Build the Rust libraries:

```bash
cargo build -p calculator
cargo build -p weaveffi-node-addon
```

2. Place the addon where the generated loader expects it:

```bash
# macOS
cp target/debug/libindex.dylib generated/node/index.node

# Linux
cp target/debug/libindex.so generated/node/index.node
```

3. Run the example:

```bash
cd examples/node

# macOS
DYLD_LIBRARY_PATH=../../target/debug npm start

# Linux
LD_LIBRARY_PATH=../../target/debug npm start
```

## Contacts (`contacts.mjs`)

1. Build the contacts library and generate Node bindings:

```bash
cargo build -p contacts
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated
```

2. Place the addon:

```bash
# macOS
cp target/debug/libindex.dylib generated/node/index.node

# Linux
cp target/debug/libindex.so generated/node/index.node
```

3. Run:

```bash
cd examples/node

# macOS
DYLD_LIBRARY_PATH=../../target/debug node contacts.mjs

# Linux
LD_LIBRARY_PATH=../../target/debug node contacts.mjs
```
