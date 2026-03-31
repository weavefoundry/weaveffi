# Guides

Practical guides for working with WeaveFFI-generated bindings across languages.

- [Memory Ownership](memory.md) — allocation rules, freeing strings, bytes, structs, and errors across the FFI boundary.
- [Error Handling](errors.md) — the uniform error model and how each target language surfaces failures.
- [Annotated Rust Extraction](extract.md) — extract an API definition from annotated Rust source instead of writing YAML by hand.
- [Generator Configuration](config.md) — customise Swift module names, Android packages, C prefixes, and other generator options via `weaveffi.toml`.
