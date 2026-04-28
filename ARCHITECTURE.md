# WeaveFFI Architecture

The canonical architecture reference lives in
[`docs/src/architecture.md`](docs/src/architecture.md).

Start there if you are:

- Adding or changing a generator.
- Touching the IDL, IR, validator, or schema version.
- Changing generator configuration, output determinism, or cache behavior.
- Reviewing snapshot-test output.

At a high level, WeaveFFI follows this pipeline:

```text
IDL (YAML/JSON/TOML)
  → Parse
  → IR
  → Validate
  → Resolve generator config
  → Generate target outputs
  → Write files and per-generator cache entries
```

The workspace is split into small crates:

- `weaveffi-ir` owns the IR and parsers.
- `weaveffi-abi` owns the stable C ABI runtime.
- `weaveffi-core` owns validation, generator orchestration, configuration,
  templates, and caching.
- `weaveffi-gen-*` crates own target-specific code generation.
- `weaveffi-cli` wires the pipeline into the `weaveffi` command.
- `weaveffi-fuzz` contains unpublished fuzz harnesses.

See the full architecture guide for the dependency graph, data-flow diagram,
cache-key strategy, generator responsibilities, and snapshot-test layout.
