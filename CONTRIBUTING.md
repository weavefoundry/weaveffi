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

## Running specific tests

```bash
cargo test -p weaveffi-core
cargo test -p weaveffi-ir
```

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
- Feature branches: `feature/...` from `main`; hotfixes: `hotfix/...` from `main`.

#### Branch naming

- Use lowercase kebab-case; no spaces; keep names concise (aim ≤ 40 chars).
- Suggested prefixes (align with Conventional Commit categories):
  - `feature/<short-desc>`
  - `fix/<short-desc>`
  - `chore/<short-desc>`
  - `docs/<short-desc>`
  - `ci/<short-desc>`
  - `refactor/<short-desc>`
  - `test/<short-desc>`
  - `perf/<short-desc>`
  - `build/<short-desc>`
  - `hotfix/<short-desc>`

Examples:

```text
feature/struct-codegen
fix/swift-string-ownership
docs/contributing-guidelines
ci/add-wasm-workflow
build/update-clap
refactor/extract-template-engine
test/calculator-fixtures
hotfix/android-jni-crash
```

## CI

- **CI** (`ci.yml`): runs `cargo fmt --check`, `cargo clippy`, `cargo test`, and build verification on macOS and Linux for every push and PR.
- **PR Lint** (`pr-lint.yml`): validates the PR title against Conventional Commits format (protects squash merges) and checks individual commit messages via commitlint (protects rebase merges).
- **Release** (`release.yml`): runs on merge to `main`; computes version, generates changelog, tags, creates GitHub Release, and publishes all workspace crates to crates.io.

## Security

- Do not commit secrets or credentials.

## License

By contributing, you agree that your contributions are licensed under the repository's MIT OR Apache-2.0 License.
