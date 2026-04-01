# Tutorial: Swift iOS App

This tutorial walks through building a Rust library, generating Swift
bindings with WeaveFFI, and integrating everything into an Xcode iOS
project.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel)
- Xcode 15+ with iOS SDK
- WeaveFFI CLI installed (`cargo install weaveffi-cli`)
- iOS Rust targets:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
```

## 1) Define your API

Create a file called `greeter.yml`:

```yaml
version: "0.1.0"
modules:
  - name: greeter
    structs:
      - name: Greeting
        fields:
          - { name: message, type: string }
          - { name: lang, type: string }
    functions:
      - name: hello
        params:
          - { name: name, type: string }
        return: string
      - name: greeting
        params:
          - { name: name, type: string }
          - { name: lang, type: string }
        return: Greeting
```

## 2) Generate bindings

```bash
weaveffi generate greeter.yml -o generated --scaffold
```

This produces:

```text
generated/
├── c/
│   └── weaveffi.h
├── swift/
│   ├── Package.swift
│   └── Sources/
│       ├── CWeaveFFI/
│       │   └── module.modulemap
│       └── WeaveFFI/
│           └── WeaveFFI.swift
└── scaffold.rs
```

## 3) Create the Rust library

Create a new Cargo project for the native library:

```bash
cargo init --lib mygreeter
```

**mygreeter/Cargo.toml:**

```toml
[package]
name = "mygreeter"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["staticlib", "cdylib"]

[dependencies]
weaveffi-abi = { version = "0.1" }
```

Use `staticlib` for iOS — Xcode links static libraries into the app
bundle. `cdylib` is included for desktop testing.

**mygreeter/src/lib.rs:**

```rust
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

#[no_mangle]
pub extern "C" fn weaveffi_greeter_hello(
    name_ptr: *const c_char,
    _name_len: usize,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    abi::error_set_ok(out_err);
    let name = unsafe { CStr::from_ptr(name_ptr) }.to_str().unwrap_or("world");
    let msg = format!("Hello, {name}!");
    CString::new(msg).unwrap().into_raw() as *const c_char
}

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr);
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len);
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err);
}
```

Fill in the remaining functions (`weaveffi_greeter_greeting`,
`weaveffi_greeter_Greeting_destroy`, getters, etc.) using the generated
`scaffold.rs` as a guide.

## 4) Build for iOS

Build the static library for each iOS target:

```bash
# Physical devices (arm64)
cargo build -p mygreeter --target aarch64-apple-ios --release

# Simulator (arm64 Apple Silicon)
cargo build -p mygreeter --target aarch64-apple-ios-sim --release

# Simulator (x86_64 Intel Mac)
cargo build -p mygreeter --target x86_64-apple-ios --release
```

Create a universal simulator library with `lipo`:

```bash
mkdir -p target/universal-ios-sim/release

lipo -create \
  target/aarch64-apple-ios-sim/release/libmygreeter.a \
  target/x86_64-apple-ios/release/libmygreeter.a \
  -output target/universal-ios-sim/release/libmygreeter.a
```

Optionally, create an XCFramework that bundles both device and simulator
slices:

```bash
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libmygreeter.a \
  -headers generated/c/ \
  -library target/universal-ios-sim/release/libmygreeter.a \
  -headers generated/c/ \
  -output MyGreeter.xcframework
```

## 5) Set up the Xcode project

1. **Create a new iOS App** in Xcode (SwiftUI or UIKit).

2. **Add the static library.** Drag `MyGreeter.xcframework` (or the
   `.a` file for a single architecture) into your project navigator.
   Ensure it appears under **Build Phases > Link Binary With Libraries**.

3. **Add the generated Swift package.** In Xcode, go to
   **File > Add Package Dependencies > Add Local…** and select
   `generated/swift/`. This adds the `CWeaveFFI` (C module map) and
   `WeaveFFI` (Swift wrapper) targets.

4. **Set the Header Search Path.** Under **Build Settings > Header
   Search Paths**, add the path to `generated/c/` (e.g.
   `$(SRCROOT)/../generated/c`). This lets the module map find
   `weaveffi.h`.

5. **Set the Library Search Path.** Under **Build Settings > Library
   Search Paths**, add the path to the Rust static library (e.g.
   `$(SRCROOT)/../target/aarch64-apple-ios/release` for device builds).

6. **Add a bridging dependency.** In your app target's
   **Build Phases > Dependencies**, ensure `WeaveFFI` is listed.

## 6) Call from Swift

```swift
import WeaveFFI

struct ContentView: View {
    @State private var greeting = ""

    var body: some View {
        VStack {
            Text(greeting)
            Button("Greet") {
                do {
                    greeting = try Greeter.hello("Swift")
                } catch {
                    greeting = "Error: \(error)"
                }
            }
        }
        .padding()
    }
}
```

The generated `WeaveFFI` module exposes:

- `Greeter.hello(_:)` — returns a `String`
- `Greeter.greeting(_:_:)` — returns a `Greeting` object with `.message`
  and `.lang` properties
- `Greeting` — a class wrapping the opaque Rust pointer, with automatic
  cleanup on `deinit`

## 7) Build and run

Select an iOS Simulator target in Xcode and press **Cmd+R**. The app
should display "Hello, Swift!" when you tap the button.

For a physical device, ensure you built for `aarch64-apple-ios` and that
the correct library search path is set.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `Undefined symbols for architecture arm64` | Check that the static library is linked and the library search path is correct. |
| `Module 'CWeaveFFI' not found` | Ensure the header search path points to `generated/c/`. |
| `No such module 'WeaveFFI'` | Add the `generated/swift/` local package to your Xcode project. |
| Simulator crash on Intel Mac | Build with `x86_64-apple-ios` and create a universal binary with `lipo`. |

## Next steps

- See the [Swift generator reference](../generators/swift.md) for type
  mapping details.
- Read the [Memory Ownership](../guides/memory.md) guide to understand
  struct lifecycle management.
- Explore the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough.
