# Node Example (N-API addon)

## Prerequisites

- Node.js 22+ installed
- Rust toolchain installed

## Steps (from repo root)

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
