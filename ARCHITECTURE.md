# WeaveFFI Architecture

The contributor guide to the architecture lives in
[`docs/src/architecture.md`](docs/src/architecture.md), and the normative C
ABI (revision 6) is [`docs/src/reference/abi.md`](docs/src/reference/abi.md).
Read them before changing a generator, the IDL or IR, validation, the
producer macros, the runtime, or the orchestrator.

The pipeline:

```text
a Rust producer crate (the API read from its built library's metadata)
or an IDL (YAML/JSON, possibly split across files with `imports:`)
  → IR (Api)
  → validate (every rule, including the C symbol table, under the Identity
    from weaveffi.toml / cargo metadata)
  → Model (one namespace: flat lists of functions, records, enums, errors,
    interfaces, and callbacks; types split by position as Ty / ParamTy /
    RetTy; every callable with its AbiFn and Shape resolved once)
  → Target::render (pure, per language, through the shared emitters in
    codegen/; every value marshalled by its one Crossing)
  → Orchestrator (a Changeset: changed-file writes, generation records in
    {out}/.weaveffi/, stale-file removal; `generate --check` and `--diff`
    report it)
```

The workspace crates:

- `weaveffi-model`: the IR, YAML and JSON parsing with imports, library
  metadata frames, validation, package identity, the model and its symbol
  table, the C type algebra, error-type naming, and the contract. It
  doesn't depend on `syn`.
- `weaveffi-cli`: the `weaveffi` command and the library behind it: the one
  `Target` trait and its `REGISTRY`, the shared emitters (`codegen::values`,
  `codecs`, `contract`, `errors`, `docs`), the orchestrator and generation
  records, packaging, and the language targets under `src/targets/`.
- `weaveffi-macros`: the per-item attributes (`export`, `record`,
  `enumeration`, `error`, `interface`, `callback_interface`, `custom`) and
  `export_runtime!`, each expanding on its own.
- `weaveffi`: the producer facade crate and the C ABI runtime (a hidden
  module) linked into every producer.
- `weaveffi-fuzz`: unpublished fuzz harnesses.
- `xtask`: the repository's task runner (`cargo xtask conformance`,
  `fixtures`, and `skew`).

The `examples/` crates hold the code the book includes, and `samples/` the
producers the conformance harness runs in every language.
