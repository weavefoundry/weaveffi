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

## Design principle: standalone generated packages

Generated packages should be fully self-contained and publishable to their
native ecosystem (npm, CocoaPods, Maven Central, PyPI, NuGet, pub.dev, etc.)
without requiring consumers to install WeaveFFI tooling, runtimes, or support
packages. WeaveFFI is a build-time tool for library authors — end users should
never need to know it exists. Any helper code (error types, memory management
utilities) is generated inline into each package rather than pulled from a
shared runtime dependency.

See the [getting started](getting-started.md) guide to try it.
