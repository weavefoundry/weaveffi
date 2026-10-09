# Contributing to WeaveFFI

## Development environment

1. Install the [Rust toolchain](https://rustup.rs/) (stable channel),
   [`just`](https://github.com/casey/just), and `cargo-insta`
   (`cargo install just cargo-insta --locked`).
2. Clone the repository:

```bash
git clone https://github.com/weavefoundry/weaveffi.git
cd weaveffi
```

3. Build the workspace:

```bash
cargo build --workspace
```

4. Run all tests:

```bash
just test        # or: cargo insta test --workspace --check
```


## Claiming an issue

To avoid duplicate work, claim an issue before you start on it:

1. Check the issue's assignee and any linked pull requests. If either exists, the issue is taken.
2. Comment on the issue to claim it and wait for a maintainer to assign it to you before opening a pull request.
3. If you stop working on an assigned issue, leave a comment so it can be reassigned.

Unsolicited pull requests for issues that are already assigned or already have an open pull request will be closed as duplicates, even if the work is good.

## Everyday workflow

The `justfile` wraps the commands CI runs:

```bash
just check                       # cargo fmt --check, clippy -D warnings (with the doc lints), rustdoc -D warnings
just test                        # cargo insta test --workspace --check: every test, failing on snapshot drift
just snapshots                   # accept snapshot changes after reviewing them
just fixtures [targets...]       # compile-check generated fixtures with each language's toolchain
just conformance                 # end-to-end conformance harness (ONLY=python just conformance)
just docs                        # build the mdBook
```

Run `just check` and `just test` before every push. Changes to a generator
should also pass that target's fixture check and conformance lanes.

### Snapshot tests

Snapshot tests (`crates/weaveffi-cli/tests/snapshots.rs`) render every
registered target for every fixture in `crates/weaveffi-cli/tests/fixtures/`
and pin the exact output for the `kitchen_sink` fixture, file by file, using
[`cargo-insta`](https://insta.rs/). Fixed files (runtimes, package
manifests, and READMEs, listed per target by `Target::fixed_files`) aren't
snapshotted, and a target's copy of the C header is asserted byte-equal to
the C target's own header instead. The other fixtures are covered by the
fixture compile checks, the determinism test, and targeted assertions in
each target's `tests.rs` (which should test only target-specific naming,
escaping, and configuration, not what the snapshot already pins). When
output changes on purpose:

```bash
cargo install cargo-insta --locked
cargo insta test --workspace     # writes .snap.new files for changed output
cargo insta review               # a accepts, r rejects, s skips
```

Review every diff. Commit accepted `.snap` files in the same commit as the
code that produced them, and never commit `.snap.new` files; CI rejects
pending snapshots.

### Fixture compile checks

Snapshots prove the text didn't change, not that it compiles.
`scripts/check-fixtures.sh <target>` generates every fixture and compiles or
type-checks it with the target language's toolchain (through
`scripts/fixtures/<target>.sh`). Run it for any target whose output you
change; CI runs one job per target. A missing tool (including `mypy` 1.x for
Python and the node-gyp headers from `npx node-gyp install` for Node.js) is
reported as a skip locally and fails the check when `CI=true`. The Python
and Ruby checks (and conformance lanes) need Python 3.10 and Ruby 3.2 or
newer: they use the `python3` and `ruby` on PATH when those are new enough,
and otherwise look for a `python3.N` on PATH or Homebrew's Ruby (see
`scripts/toolchains.sh`).

### Conformance

`conformance/run.sh` builds the three sample producers (`calculator`,
`codec`, and `kvstore`; see [Samples](docs/src/samples.md)) with leak counters
on, generates bindings, and runs real consumers in every language against
them: one lane per sample per language, named `<lang>-<sample>`, with its
consumer in `conformance/<lang>/`. Each consumer must exit 0 with every leak
counter at zero. A new feature belongs in `samples/kvstore` (or in
`samples/codec`'s vector table, for a new wire shape), with assertions in
every language's consumer.

```bash
ONLY=python bash conformance/run.sh             # one language
ONLY=c,go-kvstore bash conformance/run.sh       # a language plus one lane
SKIP=wasm SKIP_GEN=1 bash conformance/run.sh    # skip lanes; reuse generated bindings
```

`LANE_TIMEOUT` sets the per-lane limit in seconds (default 300). A missing
toolchain skips its language with a note locally and fails it when
`CI=true`.

## Documentation

- **Prose docs** live under `docs/src/` and build with
  [mdBook](https://rust-lang.github.io/mdBook/). Follow `AGENTS.md` for
  style. `scripts/check-links.sh` (needs `mdbook` and `mdbook-linkcheck2`)
  fails on broken links, and CI runs it.
- **API docs** come from Rust doc comments. Every public item in the library
  crates has one, enforced by `#![deny(missing_docs)]` and the Clippy doc
  lints (`missing_errors_doc`, `missing_panics_doc`, `missing_safety_doc`,
  `doc_markdown`). See the
  [doc comment style guide](docs/src/api/doc-style.md).

Preview the book with `mdbook serve docs -p 3000 -n 127.0.0.1`.

## Adding a new generator

Read the [architecture guide](docs/src/architecture.md) first; its "Adding a
generator" section is the checklist. In short: add
`crates/weaveffi-cli/src/targets/<lang>/` implementing `Target` (rendering
from the passing contracts stored on the model and through the shared
emitters in `codegen/`, never matching on a type's family), register
it with one entry in `REGISTRY` in `crates/weaveffi-cli/src/targets/mod.rs`
(which also adds it to `--target`, the config tables, and the snapshot
tests), add `scripts/fixtures/<lang>.sh` and `conformance/<lang>/` and list
the target in the CI matrices, `scripts/check-fixtures.sh`, and
`conformance/run.sh` (a test checks that every list matches the registry),
and document it, with its tier, under `docs/src/generators/`.

## Fuzzing

Fuzz harnesses for the parsers, the validator, and the runtime's
value-buffer decoders live in `crates/weaveffi-fuzz` and are
driven by [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) +
`libFuzzer`. They require nightly Rust because the libFuzzer sanitizer flags
are unstable.

Install once:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
```

Seed a target's corpus from its committed seeds and the snapshot fixtures
(needs `pip install pyyaml`), then run it for 60 seconds (swap the target
name for any of `fuzz_parse_yaml`, `fuzz_parse_json`, `fuzz_parse_type_ref`,
`fuzz_validate`, `fuzz_value_buffer`):

```bash
python3 crates/weaveffi-fuzz/seed_corpus.py fuzz_parse_yaml \
    crates/weaveffi-fuzz/corpus/fuzz_parse_yaml
cargo +nightly fuzz run \
    --fuzz-dir crates/weaveffi-fuzz \
    --features fuzzing \
    fuzz_parse_yaml \
    -- -max_total_time=60
```

Drop `-max_total_time=60` to fuzz indefinitely.

### Triaging a crash

When libFuzzer finds an input that panics or aborts it writes the bytes to
`crates/weaveffi-fuzz/artifacts/<target>/crash-<hash>`. To triage:

1. Pretty-print the input as the target sees it:

   ```bash
   cargo +nightly fuzz fmt \
       --fuzz-dir crates/weaveffi-fuzz \
       --features fuzzing \
       <target> \
       crates/weaveffi-fuzz/artifacts/<target>/crash-<hash>
   ```

2. Minimize the reproducer:

   ```bash
   cargo +nightly fuzz tmin \
       --fuzz-dir crates/weaveffi-fuzz \
       --features fuzzing \
       <target> \
       crates/weaveffi-fuzz/artifacts/<target>/crash-<hash>
   ```

3. Convert the minimized input into a regression test in `weaveffi-model`
   (which owns the parsers and the validator) or `weaveffi` (which owns the
   value-buffer decoders) **before** fixing the bug, so the failure is
   locked in and can't regress.

## Commit conventions

This repo uses Conventional Commits for all commits. Keep it simple: we do not use scopes.

Use the form:

```
<type>: <subject>

[optional body]

[optional footer(s)]
```

Subject rules:

- Imperative mood, no trailing period, 72 characters or fewer
- UTF-8 allowed; avoid emoji in the subject

Accepted types:

- `build`: build system or external dependencies (e.g., package.json, tooling)
- `chore`: maintenance (no app behavior change)
- `ci`: continuous integration configuration (workflows, pipelines)
- `docs`: documentation only
- `feat`: user-facing feature or capability
- `fix`: bug fix
- `perf`: performance improvements
- `refactor`: code change that neither fixes a bug nor adds a feature
- `revert`: revert of a previous commit
- `style`: formatting/whitespace (no code behavior)
- `test`: add/adjust tests only

Examples:

```text
feat: add SwiftPM scaffolding for Swift bindings
fix: correct C string ownership in Kotlin generator
docs: document memory management and error mapping
style: format generated TypeScript definitions
chore: update Gradle wrapper and Android build scripts
ci: add workflow to build Wasm target
perf: speed up header parser for large C APIs
refactor: extract template engine from codegen core
test: add fixtures for calculator sample
revert: revert "perf: speed up header parser for large C APIs"
```

Breaking changes:

- Use `!` after the type or a `BREAKING CHANGE:` footer.

```text
feat!: switch JS generator from callbacks to Promises

BREAKING CHANGE: JS bindings now return Promises instead of using callbacks; update call sites.
```

## Versioning and releases

- Every published crate shares one version, set once in the root
  `Cargo.toml` (`[workspace.package]`).
- Releases are automated by [release-plz](https://release-plz.dev/)
  (`release-plz.toml`, `.github/workflows/release.yml`). On every push to
  `main` it opens or updates a release PR that bumps the version from the
  Conventional Commits since the last release and updates `CHANGELOG.md`.
  CI runs on that PR like any other.
- Merging the release PR publishes every crate to crates.io, tags
  `v{version}`, and creates the GitHub release, which triggers
  `.github/workflows/release-binaries.yml` to attach prebuilt `weaveffi`
  binaries (the archives `cargo binstall weaveffi-cli` resolves).
- The bump follows release-plz's Conventional Commits rules for `0.x`
  versions: breaking changes bump the minor version. Only `feat`, `fix`,
  `perf`, and `revert` commits appear in the changelog.
- Don't bump versions or edit `CHANGELOG.md` by hand.

### Branching rules

- `main`: default branch.
- All work branches are created from `main`.

#### Branch naming

- Use lowercase kebab-case; no spaces; keep names concise (aim ≤ 40 chars).
- Branch prefixes match Conventional Commit types:
  - `feat/<short-desc>`
  - `fix/<short-desc>`
  - `chore/<short-desc>`
  - `docs/<short-desc>`
  - `ci/<short-desc>`
  - `refactor/<short-desc>`
  - `test/<short-desc>`
  - `perf/<short-desc>`
  - `build/<short-desc>`

Examples:

```text
feat/struct-codegen
fix/swift-string-ownership
docs/contributing-guidelines
ci/add-wasm-workflow
build/update-clap
refactor/extract-template-engine
test/calculator-fixtures
fix/android-jni-crash
```

## CI

- **CI** (`ci.yml`): formatting, clippy, rustdoc, and a build of
  `weaveffi-model` without its IDL features; the test suite with snapshot
  checks on Linux, macOS, and Windows; `weaveffi generate --check` on
  every sample, the JSON Schema drift check, and a `wasm32` build of every sample
  whose embedded metadata must match the native build's; the fixture compile
  check per target; the conformance harness per language on Linux and macOS;
  an Android job that links the Kotlin JNI shim with the NDK and runs
  `weaveffi build` for Android; and a package install smoke test
  (`scripts/package-smoke.sh`) on Linux and macOS.
- **Quality** (`quality.yml`): `cargo deny`, `cargo audit`, `cargo machete`,
  coverage with `cargo llvm-cov`, and the docs link check.
- **Docs** (`docs.yml`): builds and deploys the mdBook and rustdoc.
- **Bench** (`bench.yml`): a weekly sampled benchmark run, uploaded as an
  artifact. Pull requests run each benchmark once in `ci.yml`.
- **Fuzz** (`fuzz.yml`): a daily 60-second run of every fuzz target from a
  corpus seeded with the committed seeds and the snapshot fixtures.
- **PR Lint** (`pr-lint.yml`): checks the PR title and commit messages
  against Conventional Commits.
- **Release** (`release.yml`) and **Release binaries**
  (`release-binaries.yml`): see above.

## Security

- Do not commit secrets or credentials.

## License

By contributing, you agree that your contributions are licensed under the repository's MIT OR Apache-2.0 License.
