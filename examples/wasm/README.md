Minimal Wasm example

1. Build the calculator crate for wasm:

```
cargo build -p calculator --target wasm32-unknown-unknown --release
```

2. Serve `weaveffi_wasm.js` and the built `.wasm` from `target/wasm32-unknown-unknown/release/`.

Use a simple static server and import the loader to instantiate the module.
