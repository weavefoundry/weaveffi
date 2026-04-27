# WeaveFFI WASM browser example

This example runs the calculator sample in the browser. The main page starts a
module worker, the worker imports the generated `weaveffi_wasm.js` loader, and
the UI calls `calculator.add`, `calculator.echo`, and `calculator.div` to show
error handling.

## Build

From the repository root:

```sh
./examples/wasm/browser/build.sh
```

The build script first compiles the calculator cdylib for
`wasm32-unknown-unknown`, then generates the WASM JavaScript wrapper into
`examples/generated/wasm/`.

Equivalent commands:

```sh
cargo build -p calculator --target wasm32-unknown-unknown --release
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o examples/generated --target wasm
```

## Run

Serve the repository root so the example can load both `examples/generated/` and
`target/`:

```sh
./examples/wasm/browser/serve.sh
```

Then open:

```text
http://localhost:8080/examples/wasm/browser/
```

`serve.sh` is a small wrapper around `python3 -m http.server 8080`.
