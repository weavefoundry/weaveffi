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
