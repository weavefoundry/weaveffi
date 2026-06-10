# Stability and Versioning

WeaveFFI follows [Semantic Versioning](https://semver.org/) once it reaches
1.0.0. Until then it is in active pre-1.0 development and **any** surface area
may change between minor versions. This page documents exactly what is — and
isn't — covered, what the deprecation policy will look like post-1.0, and how
to bind your CI to a stable WeaveFFI workflow today.

## What semver covers (post-1.0)

After the 1.0.0 release, the following surfaces will be governed by SemVer:

- **CLI flags and subcommands.** Every documented `weaveffi <subcommand>`,
  every flag, every exit code, and every documented stdout/stderr format
  (`--format json` payloads in particular). Adding a new optional flag is a
  minor bump; removing or renaming one is a breaking change.
- **IDL schema.** The set of accepted top-level keys, type-reference syntax
  (`handle<T>`, `iter<T>`, `[T]`, `{K:V}`, `T?`, `&str`, `&[u8]`, primitives,
  user-defined struct/enum names), `version` semantics, and the JSON Schema
  exported by `weaveffi schema --format json-schema`.
- **Generated code shape.** The exported symbol names, function signatures,
  type names, package layouts, and ABI conventions of every generator's
  output. A patch release will not change the bytes of the generated output;
  a minor release may add new symbols but will not remove or rename existing
  ones; a major release may break.
- **Public Rust API of every published crate.** That is `weaveffi-ir`,
  `weaveffi-abi`, `weaveffi-core`, `weaveffi-gen-c`, `weaveffi-gen-cpp`,
  `weaveffi-gen-swift`, `weaveffi-gen-android`, `weaveffi-gen-node`,
  `weaveffi-gen-wasm`, `weaveffi-gen-python`, `weaveffi-gen-dotnet`,
  `weaveffi-gen-dart`, `weaveffi-gen-go`, `weaveffi-gen-ruby`, and
  `weaveffi-cli`. The `Generator` trait, the `Orchestrator`, the IR types,
  and the C ABI runtime symbols exported from `weaveffi-abi` are all public
  contracts.

## What is NOT covered pre-1.0

While the workspace is at `0.x`, **everything** above may change without
warning. In practice we try to keep breaking changes batched (one batch per
minor release, with a schema-version bump), but the contract is "no
contract." Things that have already changed during 0.x:

- IR type-reference syntax (`callback` was removed in `0.3.0`).
- The `Generator` trait gained `generate_with_config` in `0.3.0`, then
  was reworked in `0.5.0` into an associated `Config` type (with an
  object-safe `DynGenerator` view) that replaced the
  `*_with_config` method pair. A prototype Tera template hook
  (`generate_with_templates`, `--templates`, `template_dir`) was added
  and then removed in `0.4.0` because no generator ever consumed it.
- The C ABI runtime added `weaveffi_arena_*` and `weaveffi_cancel_token_*`
  families.
- `weaveffi doctor` gained `--target` and `--format json`.

Pin the WeaveFFI version in CI (`cargo install weaveffi-cli --version
=0.3.0`) and vendor the generated output in your repository so that
upgrades are an explicit, reviewable event.

## Post-1.0 deprecation policy

Once we reach 1.0.0, breaking changes will follow this path:

1. The feature is marked deprecated in a minor release. The CLI prints a
   `--warn`-style diagnostic (`weaveffi: warning: <name> is deprecated;
   <suggested replacement>`) on every invocation that touches it. The
   generators emit a native deprecation marker where the target language
   supports one (`#[deprecated]` in Rust, `@Deprecated` in Kotlin/Java,
   `@available(*, deprecated:)` in Swift, `[Obsolete]` in .NET, JSDoc
   `@deprecated` in TypeScript, and so on — driven by the existing IDL
   `deprecated:` field).
2. The deprecated feature continues to work for **at least one full minor
   version**.
3. Removal lands in the next major release with a migration note in
   `CHANGELOG.md`.

In short: nothing disappears in a patch release, nothing disappears without
at least one minor release of warnings, and every removal ships with a
documented replacement.

## IR schema version policy

The IR schema version is independent of the workspace version, but it is
tied to `weaveffi-ir`'s minor version: each `weaveffi-ir` minor bump
corresponds to at most one schema version bump.
[`CURRENT_SCHEMA_VERSION`](https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-ir/src/ir.rs)
in `crates/weaveffi-ir/src/ir.rs` is the source of truth.

Pre-1.0, **only the current schema version is accepted**
(`SUPPORTED_VERSIONS` contains exactly `CURRENT_SCHEMA_VERSION`). When a
schema bump lands, update the `version` field in your IDL and adjust the
document to the new schema — the changes are documented in `CHANGELOG.md`
with a "Migration" section. Post-1.0, schema bumps will ship with an
automated migration tool and a widened `SUPPORTED_VERSIONS` window.

## Generated-code stability (determinism)

> **Regenerating with the same WeaveFFI version on the same IDL produces
> byte-identical output.**

This is enforced by the determinism tests: every generator's output is
hashed and re-hashed on the kitchen-sink fixture, and any deviation fails
CI. Internally, every
`HashMap` iteration that contributes to generated output has been replaced
by `BTreeMap` or an explicit sort. The `serde_json`-backed cache key uses a
canonical key ordering.

Practical consequences:

- Vendoring the generated `bindings/` directory in your repository is
  safe. A reviewer will only see a diff when the IDL or the generator
  itself changes.
- `weaveffi diff --check` (see below) is a reliable CI gate.
- Cross-platform regeneration (Linux vs macOS vs Windows) produces the
  same bytes for the same WeaveFFI version.

If you ever observe non-determinism, please file an issue with the IDL
that triggers it — it's a bug, not a quirk.

## The `weaveffi diff --check` workflow for downstream CI

The single recommended way to guard a downstream repository against
"forgot to regenerate" mistakes is `weaveffi diff --check`:

```bash
weaveffi diff path/to/api.yml --out generated/ --check
```

`diff --check` regenerates into a temporary directory, compares against
`--out`, and exits:

- **0** when the on-disk output matches what regeneration would produce,
- **2** when at least one file differs (modified content),
- **3** when files are missing or extra (a target was added/removed).

It prints only the summary `+ N added, - M removed, ~ K modified` —
suitable for CI logs without flooding the output.

A typical GitHub Actions step:

```yaml
- name: Verify generated bindings are up to date
  run: |
    cargo install weaveffi-cli --locked --version =0.3.0
    weaveffi diff idl/api.yml --out generated/ --check
```

Combine it with `weaveffi format --check idl/api.yml` (canonical IDL) and
`weaveffi validate idl/api.yml` (schema correctness) for a complete CI
guard.

## See also

- [IDL Schema](reference/idl.md) — the type system the schema version
  governs.
- [Getting Started](getting-started.md) — installation and the basic
  workflow `diff --check` plugs into.
