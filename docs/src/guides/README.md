# Guides

Practical guides for working with WeaveFFI bindings across targets.

- [The Rust Producer Macro](producer-macro.md): write safe Rust, annotate it with `#[weaveffi::module]`, and let the `weaveffi` crate generate the C ABI glue.
- [Memory Ownership](memory.md): allocation rules; freeing strings, bytes, structs, and errors across the FFI boundary.
- [Error Handling](errors.md): the uniform error model and how each target surfaces failures.
- [Async Functions](async.md): IDL declaration, the C ABI callback contract, and per-target async surfaces.
- [Annotated Rust Extraction](extract.md): extract an IDL from annotated Rust source instead of writing YAML by hand.
- [Generator Configuration](config.md): customize per-target names, module-prefix stripping, and the C ABI prefix via `weaveffi.toml` or inline `generators:` blocks.
- [Packaging and Distribution](packaging.md): assemble ready-to-publish packages that bundle prebuilt native libraries per platform.
