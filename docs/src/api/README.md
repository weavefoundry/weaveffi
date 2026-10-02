# Rust Crates

WeaveFFI is published as six crates that share one version:

| Crate | Use it when |
|-------|-------------|
| [`weaveffi`](https://docs.rs/weaveffi) | You're writing a Rust producer. It's the only dependency a producer needs. |
| [`weaveffi-abi`](https://docs.rs/weaveffi-abi) | You need the runtime directly; producers reach it as `weaveffi::abi`. |
| [`weaveffi-macros`](https://docs.rs/weaveffi-macros) | Never directly; `weaveffi` re-exports its macros. |
| [`weaveffi-model`](https://docs.rs/weaveffi-model) | You're building a tool on the IR: parsing, validating, or extracting an API, or reading the binding model. |
| [`weaveffi-gen`](https://docs.rs/weaveffi-gen) | You're driving or extending the generators from Rust. |
| [`weaveffi-cli`](https://crates.io/crates/weaveffi-cli) | You want the `weaveffi` command (`cargo install weaveffi-cli`). |

- [Rust API Map](rust.md) lists the items a producer uses from `weaveffi`
  and `weaveffi-abi`, and which exist only for the macro expansion.
- [Doc Comment Style](doc-style.md) is the convention for the doc comments
  themselves.

The full API docs are published at
[weaveffi.com/api/rust/weaveffi/](https://weaveffi.com/api/rust/weaveffi/)
and build locally with:

```bash
cargo doc --workspace --all-features --no-deps --open
```

Every public item in the library crates is documented, enforced by
`#![deny(missing_docs)]` and the Clippy doc lints.
