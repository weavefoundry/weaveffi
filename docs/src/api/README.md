# API

Reference documentation for the WeaveFFI Rust crates.

- [Rust API](rust.md): a map of the public surface of the `weaveffi` facade
  (the attributes, `export_runtime!`, `Iter`, `CancelToken`, `ErrorReport`,
  and the spawner) and of the `weaveffi-abi` runtime it re-exports (the
  error struct and reserved codes, reference-counted objects, callback
  vtables, the value-buffer codec, cancel tokens, and the async spawner),
  with a note on which items a producer calls and which exist for the macro
  expansion.
- [Doc Comment Style](doc-style.md): the conventions and lints behind the
  doc comments themselves.

API docs are generated from source via `cargo doc`:

```bash
cargo doc --workspace --all-features --no-deps --open
```

When the documentation site is deployed, API docs are available under the
[API section](https://weavefoundry.github.io/weaveffi/api/rust/weaveffi/).

Every public item in the library crates is documented; this is enforced in
CI by `#![deny(missing_docs)]` and the Clippy doc lints.
