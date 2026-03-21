# Samples

This repo includes sample projects to showcase end-to-end usage.

## Calculator (Rust crate)

Path: `samples/calculator`

Build the Rust library:

```bash
cargo build -p calculator
```

Generate bindings from the calculator IR:

```bash
weaveffi generate samples/calculator/calculator.yml -o generated
```

This produces target-specific output under `generated/` (C headers, Swift
wrapper, Android skeleton, Node addon loader, WASM stub). Runnable examples
that consume the generated output are in `examples/`.

## Node addon

Path: `samples/node-addon`

An N-API addon crate that loads the calculator's C ABI symbols and exposes
them as JS-friendly functions. Used by the Node example.
