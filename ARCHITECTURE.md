# WeaveFFI Architecture

The contributor guide to the architecture lives in
[`docs/src/architecture.md`](docs/src/architecture.md), and the normative C
ABI is [`docs/src/reference/abi.md`](docs/src/reference/abi.md). Read them
before changing a generator, the IDL or IR, validation, the producer macro,
the runtime, or the orchestrator.

The pipeline:

```text
annotated Rust (.rs) or IDL (YAML/JSON/TOML)
  → IR (Api)
  → validate (every rule, including the C symbol table)
  → ResolvedApi + Identity (names from weaveffi.toml / Cargo.toml)
  → BindingModel (C symbols and ABI signatures, computed once)
  → marshalling plan
  → Target::render (pure, per language)
  → Orchestrator (capability gate, cache records, changed-file writes,
    stale-file removal)
```

The workspace crates:

- `weaveffi-model`: the IR, IDL parsing, Rust extraction, validation, the
  resolved view, package identity, the binding model, ABI lowering, the
  marshalling plan, and contract checksums.
- `weaveffi-gen`: the backend framework, the orchestrator and cache, and the
  eleven language targets under `targets/`.
- `weaveffi-cli`: the `weaveffi` command.
- `weaveffi-abi`: the C ABI runtime linked into every producer.
- `weaveffi-macros`: `#[weaveffi::module]` and `export_runtime!`.
- `weaveffi`: the producer facade crate.
- `weaveffi-fuzz`: unpublished fuzz harnesses.
