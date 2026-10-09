# WeaveFFI Architecture

The contributor guide to the architecture lives in
[`docs/src/architecture.md`](docs/src/architecture.md), and the normative C
ABI is [`docs/src/reference/abi.md`](docs/src/reference/abi.md). Read them
before changing a generator, the IDL or IR, validation, the producer macro,
the runtime, or the orchestrator.

The pipeline:

```text
a Rust producer crate (the API read from its built library's metadata)
or an IDL (YAML/JSON/TOML)
  → IR (Api)
  → validate (every rule, including the C symbol table, under the
    Identity from weaveffi.toml / cargo metadata)
  → Model (C symbols and ABI signatures, computed once; the plan)
  → Target::render (pure, per language)
  → Orchestrator (changed-file writes, generation records, stale-file
    removal)
```

The workspace crates:

- `weaveffi-model`: the IR, IDL parsing, the macro's Rust extraction,
  library metadata frames, validation, package identity, the model, ABI
  lowering, the marshalling plan, and contract tables.
- `weaveffi-cli`: the `weaveffi` command and the library behind it: the
  backend framework, the orchestrator and generation records, packaging, and
  the eleven language targets under `src/targets/`.
- `weaveffi-macros`: `#[weaveffi::module]` and `export_runtime!`.
- `weaveffi`: the producer facade crate and the C ABI runtime
  (`weaveffi::abi`) linked into every producer.
- `weaveffi-fuzz`: unpublished fuzz harnesses.
