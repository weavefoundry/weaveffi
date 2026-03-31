# WeaveFFI

WeaveFFI is a toolkit for generating cross-language FFI bindings and
language-specific packages from a concise API definition. It works with any
native library that exposes a stable C ABI — whether written in Rust, C, C++,
Zig, or another language. This book covers the concepts, setup, and end-to-end
workflows.

- Goals: strong safety model, clear memory ownership, ergonomic bindings.
- Targets: C, Swift, Android (JNI), Node.js, and Web/WASM.
- Implementation: define your API once; WeaveFFI generates the C ABI contract
  and idiomatic wrappers for each platform.

See the [roadmap](roadmap.md) for high-level milestones and the
[getting started](getting-started.md) guide to try it.
