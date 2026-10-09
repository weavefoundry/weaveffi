# WeaveFFI Architecture

The contributor guide to the architecture lives in
[`docs/src/architecture.md`](docs/src/architecture.md), and the normative C
ABI (revision 5) is [`docs/src/reference/abi.md`](docs/src/reference/abi.md).
Read them before changing a generator, the IDL or IR, validation, the
producer macro, the runtime, or the orchestrator.

The pipeline:

```text
a Rust producer crate (the API read from its built library's metadata)
or an IDL (YAML/JSON)
  → IR (Api)
  → validate (every rule, including the C symbol and slot tables, under
    the Identity from weaveffi.toml / cargo metadata)
  → Model (v2: types split by position as Ty / ParamTy / RetTy; every
    binding stores its AbiFn and its passing contracts, ArgPass, RetPass,
    ResultPass, ItemPass, CallbackRetPass, and ErrorStrategy, resolved once)
  → Target::render (pure, per language, through the shared emitters in
    codegen/; never re-derives a family, slot, or error domain)
  → Orchestrator (a Changeset: changed-file writes, generation records,
    stale-file removal; `generate --check` and `--diff` report it)
```

The workspace crates:

- `weaveffi-model`: the IR, YAML and JSON parsing, library metadata frames,
  validation, package identity, the model and its symbol table, C ABI
  lowering, the passing contracts (`plan`), error-type naming, and contract
  tables. It doesn't depend on `syn`.
- `weaveffi-cli`: the `weaveffi` command and the library behind it: the one
  `Target` trait and its `REGISTRY`, the shared emitters (`codegen::codecs`,
  `contract`, `errors`, `docs`), the orchestrator and generation records,
  packaging, and the eleven language targets under `src/targets/`.
- `weaveffi-macros`: `#[weaveffi::module]`, the marker attributes, and
  `export_runtime!`; its `src/extract.rs` lowers a module tree to the IR.
- `weaveffi`: the producer facade crate and the C ABI runtime
  (`weaveffi::abi`) linked into every producer.
- `weaveffi-fuzz`: unpublished fuzz harnesses.
