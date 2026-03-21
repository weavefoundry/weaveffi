# Node

The Node generator produces a CommonJS loader and `.d.ts` type definitions
for your functions. The generated addon uses the [N-API](https://nodejs.org/api/n-api.html)
(Node-API) interface to load the C ABI symbols and expose JS-friendly functions.

## Generated artifacts

- `generated/node/index.js` — CommonJS loader that requires `./index.node`
- `generated/node/types.d.ts` — function signatures inferred from your IR
- `generated/node/package.json`

## Running the example

### macOS

```bash
cargo build -p calculator
cp target/debug/libindex.dylib generated/node/index.node

cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

### Linux

```bash
cargo build -p calculator
cp target/debug/libindex.so generated/node/index.node

cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

## Notes

- The loader expects the compiled N-API addon next to it as `index.node`.
- The N-API addon crate is in `samples/node-addon`.
