# Rust Crates

WeaveFFI is published as four crates that share one version:

| Crate | Use it when |
|-------|-------------|
| [`weaveffi`](https://docs.rs/weaveffi) | You're writing a Rust producer. It's the only dependency a producer needs: the attributes, the few types a producer names, and the runtime the attributes expand against. |
| [`weaveffi-macros`](https://docs.rs/weaveffi-macros) | Never directly; `weaveffi` re-exports its macros. |
| [`weaveffi-model`](https://docs.rs/weaveffi-model) | You're building a tool on the IR: parsing (with imports), validating, or extracting an API, reading library metadata, or reading the validated `Model`. Its default `idl` feature adds the YAML and JSON formats, the JSON Schema, and rich diagnostics. |
| [`weaveffi-cli`](https://docs.rs/weaveffi-cli) | You want the `weaveffi` command (`cargo install weaveffi-cli`), or you're driving or extending the generators from Rust (`project::Project` loads a project, from a `build.rs` too). |

- [Rust API Map](rust.md) lists the items a producer uses from `weaveffi`,
  and the entry points of the other crates.
- [Doc Comment Style](doc-style.md) is the convention for the doc comments
  themselves.

The full API docs are published at
[weaveffi.com/api/rust/weaveffi/](https://weaveffi.com/api/rust/weaveffi/)
(under each version's path too, `/v{major}.{minor}/api/rust/`) and build
locally with:

```bash
cargo doc --workspace --all-features --no-deps --open
```

Every public item in the library crates is documented, enforced by
`#![deny(missing_docs)]` and the Clippy doc lints.
