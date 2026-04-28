# Tutorials

Each tutorial follows the same shape: **Goal**, **Prerequisites**,
**Step-by-step**, **Verification**, **Cleanup**, **Next steps**. Pick
the target you're shipping to and follow it end-to-end.

- [Calculator](calculator.md) — fastest path: generate every target,
  build the cdylib, run the C/Node/Swift consumers from the in-tree
  sample.
- [Swift iOS](swift.md) — Rust → SwiftPM → Xcode iOS app.
- [Android](android.md) — Rust → AAR → Android Studio app on
  emulator/device.
- [Python](python.md) — Rust → ctypes package → `pip install` and
  `python demo.py`.
- [Node.js](node.md) — Rust → N-API addon → `npm publish` shape.
