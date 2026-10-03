# Build the CLI
build:
    cargo build --release -p weaveffi-cli

# Run every test once (fails on snapshot drift)
test:
    cargo insta test --workspace --check

# Accept intentional snapshot changes after reviewing them
snapshots:
    cargo insta test --workspace --accept

# Formatting, clippy (which also enforces the doc lints), and rustdoc
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    RUSTDOCFLAGS="-D warnings -D rustdoc::all -D rustdoc::missing_crate_level_docs" cargo doc --workspace --no-deps

# Run the end-to-end conformance harness (ONLY=python just conformance)
conformance:
    bash conformance/run.sh

# Compile-check every snapshot fixture with each language's toolchain
fixtures *targets:
    bash scripts/check-fixtures.sh {{targets}}

# Build the mdBook site
docs:
    mdbook build docs

# Format all code
fmt:
    cargo fmt --all
