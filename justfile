# Build the CLI
build:
    cargo build --release -p weaveffi-cli

# Generate bindings from the calculator sample
generate: build
    cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated

# Run all tests
test:
    cargo test --workspace

# Check formatting and lints (clippy -D warnings also enforces the doc lints:
# missing_docs, missing_errors_doc, missing_panics_doc, missing_safety_doc,
# and doc_markdown)
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Build the Rust API docs (rustdoc) and the mdBook site. Uses the same
# RUSTDOCFLAGS as the CI rustdoc job so broken intra-doc links and missing
# crate-level docs fail locally too.
doc:
    RUSTDOCFLAGS="-D rustdoc::all -D rustdoc::missing_crate_level_docs" cargo doc --workspace --no-deps
    mdbook build docs

# Format all code
fmt:
    cargo fmt --all

# Clean generated output and build artifacts
clean:
    rm -rf generated/
    cargo clean
