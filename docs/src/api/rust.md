# Rust API (cargo doc)

## The `weaveffi` producer crate

A Rust producer depends on a single crate, [`weaveffi`][weaveffi-crate]. It
re-exports the `#[weaveffi::module]` family of attributes and the
`export_runtime!` macro, plus the C ABI runtime as `weaveffi::abi`. Annotate a
normal module, tag the items to export, and the macro emits the
`#[no_mangle] extern "C"` thunks for you. See
[The Rust Producer Macro](../guides/producer-macro.md) for the full guide.

```toml
[dependencies]
weaveffi = "0.12"
```

The supporting crates are published separately and are useful when you need
the lower layers directly:

| Crate | What it is |
|-------|------------|
| [`weaveffi`][weaveffi-crate] | The producer facade: the attribute macros, `export_runtime!`, and `abi`. Depend on this. |
| [`weaveffi-abi`][abi-crate] | The stable C ABI runtime: `weaveffi_error`, memory helpers, cancel tokens, the arena, and the `lift_*`/`lower_*` marshalling converters the macro calls. The macro generates code against it; a producer reaches the same helpers through `weaveffi::abi` when it needs one directly (for example to dereference a raw handle). |
| [`weaveffi-macros`][macros-crate] | The proc-macro implementation behind `weaveffi`'s attributes. You rarely depend on it directly. |
| [`weaveffi-ir`][ir-crate] | The IR types (`Api`, `Module`, `TypeRef`, ...) and the IDL parser. |

## Browsing the docs

Generate and view the Rust API docs locally:

```bash
cargo doc --workspace --all-features --no-deps --open
```

When the documentation site is deployed, API docs are available at
[weavefoundry.github.io/weaveffi/api/rust/weaveffi_core/](https://weavefoundry.github.io/weaveffi/api/rust/weaveffi_core/).

[weaveffi-crate]: https://docs.rs/weaveffi
[abi-crate]: https://docs.rs/weaveffi-abi
[macros-crate]: https://docs.rs/weaveffi-macros
[ir-crate]: https://docs.rs/weaveffi-ir
