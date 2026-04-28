# Contributing to WeaveFFI

## Development environment

1. Install the [Rust toolchain](https://rustup.rs/) (stable channel).
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
cargo test --workspace
```

5. (Optional) Preview the documentation locally:

```bash
cargo install mdbook
mdbook serve docs -p 3000 -n 127.0.0.1
```

Open <http://127.0.0.1:3000>.

## Running specific tests

```bash
cargo test -p weaveffi-core
cargo test -p weaveffi-ir
```

## Adding a new generator

Each target language is implemented as its own crate (`weaveffi-gen-<lang>`)
that implements the `Generator` trait from `weaveffi_core::codegen`. Before
starting, read the [architecture overview](docs/src/architecture.md) for the
crate dependency graph, the IDL → IR → Validate → Resolve → Generate → Output
data flow, and the snapshot-test layout new generators must hook into.

The short version:

1. Create `crates/weaveffi-gen-<lang>/` following the layout of an existing
   generator (e.g. `weaveffi-gen-c`). Add it to the workspace `members` list
   in the root `Cargo.toml` and depend on `weaveffi-core` and `weaveffi-ir`.
2. Implement `Generator` (`name`, `generate`, and the optional
   `generate_with_config` / `generate_with_templates` /
   `output_files` / `output_files_with_config` overrides).
3. Wire the generator into `crates/weaveffi-cli/src/main.rs` so the
   `--targets` flag accepts it.
4. Add snapshot fixtures under
   `crates/weaveffi-cli/tests/snapshots.rs` covering at minimum the
   `calculator`, `contacts`, `inventory`, `async_demo`, and `events` sample
   IDLs.
5. Document the generator under `docs/src/generators/<lang>.md` and link it
   from `docs/src/SUMMARY.md`.

## Snapshot tests

Snapshot tests are the primary defense against generator regressions. They
live in `crates/weaveffi-cli/tests/snapshots.rs` and write
one-file-per-snapshot artifacts under `crates/weaveffi-cli/tests/snapshots/`
using [`cargo-insta`](https://insta.rs/).

Workflow when output changes intentionally:

```bash
cargo install cargo-insta --locked
cargo test -p weaveffi-cli --test snapshots
cargo insta review
```

`cargo insta review` opens an interactive TUI showing the `.snap.new` diff
for each pending snapshot. Inspect every diff carefully:

- Press `a` (or run `cargo insta accept`) to promote `.snap.new` files into
  their final `.snap` form when the change is correct.
- Press `r` (or run `cargo insta reject`) to delete the `.snap.new` files
  when the diff exposes a bug — fix the generator before re-running.
- Press `s` to skip a snapshot and decide later.

After accepting, **commit the resulting `.snap` files in the same commit as
the code change that produced them** so reviewers can see the generator diff
alongside the implementation diff. Never commit `.snap.new` files; CI rejects
them.

## Fuzzing

Parser and validator fuzz harnesses live in `crates/weaveffi-fuzz` and are
driven by [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) +
`libFuzzer`. They require nightly Rust because the libFuzzer sanitizer flags
are unstable.

Install once:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
```

Run a target for 60 seconds (swap the target name for any of `fuzz_parse_yaml`,
`fuzz_parse_json`, `fuzz_parse_toml`, `fuzz_parse_type_ref`, `fuzz_validate`):

```bash
cargo +nightly fuzz run \
    --fuzz-dir crates/weaveffi-fuzz \
    --features fuzzing \
    fuzz_parse_yaml \
    crates/weaveffi-fuzz/fuzz/seeds/fuzz_parse_yaml \
    -- -max_total_time=60
```

Drop `-max_total_time=60` to fuzz indefinitely.

### Triaging a crash

When libFuzzer finds an input that panics or aborts it writes the bytes to
`crates/weaveffi-fuzz/fuzz/artifacts/<target>/crash-<hash>`. To triage:

1. Pretty-print the input as the target sees it:

   ```bash
   cargo +nightly fuzz fmt \
       --fuzz-dir crates/weaveffi-fuzz \
       --features fuzzing \
       <target> \
       crates/weaveffi-fuzz/fuzz/artifacts/<target>/crash-<hash>
   ```

2. Minimize the reproducer:

   ```bash
   cargo +nightly fuzz tmin \
       --fuzz-dir crates/weaveffi-fuzz \
       --features fuzzing \
       <target> \
       crates/weaveffi-fuzz/fuzz/artifacts/<target>/crash-<hash>
   ```

3. Convert the minimized input into a regression test in the affected crate
   (`weaveffi-ir` for parser crashes, `weaveffi-core` for validator crashes)
   **before** fixing the bug, so the failure is locked in and cannot regress.

## Commit conventions

This repo uses Conventional Commits for all commits. Keep it simple: we do not use scopes.

Use the form:

```
<type>: <subject>

[optional body]

[optional footer(s)]
```

Subject rules:

- Imperative mood, no trailing period, ≤ 72 characters
- UTF‑8 allowed; avoid emoji in the subject

Accepted types:

- `build` – build system or external dependencies (e.g., package.json, tooling)
- `chore` – maintenance (no app behavior change)
- `ci` – continuous integration configuration (workflows, pipelines)
- `docs` – documentation only
- `feat` – user-facing feature or capability
- `fix` – bug fix
- `perf` – performance improvements
- `refactor` – code change that neither fixes a bug nor adds a feature
- `revert` – revert of a previous commit
- `style` – formatting/whitespace (no code behavior)
- `test` – add/adjust tests only

Examples:

```text
feat: add SwiftPM scaffolding for Swift bindings
fix: correct C string ownership in Kotlin generator
docs: document memory management and error mapping
style: format generated TypeScript definitions
chore: update Gradle wrapper and Android build scripts
ci: add workflow to build WASM target
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

- All crates are versioned in lockstep. Versions are tracked in each `crates/*/Cargo.toml` and updated automatically by [semantic-release](https://semantic-release.gitbook.io/) via `scripts/update-cargo-versions.sh`.
- **Automated release pipeline** (on every merge to `main`):
  1. `semantic-release` scans Conventional Commit messages since the last tag.
  2. It determines the next SemVer bump: `feat` → **minor**, `fix`/`perf` → **patch**, `BREAKING CHANGE` → **minor** (while version < 1.0; see note below).
  3. `CHANGELOG.md` is generated, `Cargo.toml` versions are updated, and a tagged release commit (`chore(release): vX.Y.Z`) is pushed.
  4. All publishable crates are published to [crates.io](https://crates.io) in dependency order.
  5. A GitHub Release is created with auto-generated release notes.
- Commit types that trigger a release: `feat` (minor), `fix` and `perf` (patch), `BREAKING CHANGE` (minor while pre-1.0). All other types (`build`, `chore`, `ci`, `docs`, `refactor`, `revert`, `style`, `test`) are recorded in the changelog but do **not** trigger a release on their own.
- **Pre-1.0 breaking changes**: The `{ "breaking": true, "release": "minor" }` rule in `.releaserc.json` caps breaking changes to a minor bump. When the project is ready for 1.0.0, remove that rule so breaking changes bump major as normal.
- Tag format: `v`-prefixed (e.g., `v0.1.0`).
- Manual version bumps are no longer needed — just merge PRs with valid Conventional Commit titles. For ad-hoc runs, use the workflow's **Run workflow** button (`workflow_dispatch`).

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

- **CI** (`ci.yml`): runs `cargo fmt --check`, `cargo clippy`, `cargo test`, and build verification on macOS and Linux for every push and PR.
- **PR Lint** (`pr-lint.yml`): validates the PR title against Conventional Commits format (protects squash merges) and checks individual commit messages via commitlint (protects rebase merges).
- **Release** (`release.yml`): runs on merge to `main`; computes version, generates changelog, tags, creates GitHub Release, and publishes all workspace crates to crates.io.

## Security

- Do not commit secrets or credentials.

## License

By contributing, you agree that your contributions are licensed under the repository's MIT OR Apache-2.0 License.
