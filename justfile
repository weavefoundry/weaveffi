# Build the CLI
build:
    cargo build --release -p weaveffi-cli

# Generate bindings from the calculator sample
generate: build
    cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml -o generated

# Run all tests
test:
    cargo test --workspace

# Check formatting and lints
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Format all code
fmt:
    cargo fmt --all

# Clean generated output and build artifacts
clean:
    rm -rf generated/
    cargo clean
