# API

Reference documentation for the WeaveFFI Rust crates.

API docs are generated from source via `cargo doc`:

```bash
cargo doc --workspace --all-features --no-deps --open
```

When the documentation site is deployed, API docs are available under the
[API section](https://weavefoundry.github.io/weaveffi/api/rust/weaveffi_core/).

Every public item in the library crates is documented; this is enforced in
CI. See [Doc Comment Style](doc-style.md) for the conventions and the lints
that back them.
