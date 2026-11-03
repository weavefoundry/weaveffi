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
    cargo xtask conformance

# Compile-check every snapshot fixture with each language's toolchain
fixtures *targets:
    cargo xtask fixtures {{targets}}

# Check bindings against a library of a different version
skew:
    cargo xtask skew

# Build the mdBook site
docs:
    mdbook build docs

# Format all code
fmt:
    cargo fmt --all
