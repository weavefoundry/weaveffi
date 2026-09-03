# Tutorials

Each tutorial follows the same shape: **Goal**, **Prerequisites**,
**Step-by-step**, **Verification**, **Cleanup**, **Next steps**. Pick
the target you're shipping to and follow it end-to-end.

- [Calculator](calculator.md): fastest path to generate every target,
  build the cdylib from the in-tree sample, extract and validate its IDL,
  and run small C/Node/Swift consumers against it.
- [Swift iOS](swift.md): Rust → SwiftPM → Xcode iOS app, with a macOS
  smoke test first.
- [Kotlin](kotlin.md): Rust → `weaveffi package` → Gradle library module
  with the cdylib bundled per Android ABI → Android Studio app, with a
  desktop JVM smoke test first.
- [Python](python.md): Rust → ctypes package → `pip install` and
  `python demo.py`.
- [Node.js](node.md): Rust → N-API addon → `npm publish` shape.

The four greeter tutorials share one IDL and one `#[weaveffi::module]`
crate, so you can generate every target from a single source and follow
them in any order.
