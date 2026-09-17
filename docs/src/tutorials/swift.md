# Swift iOS App

## Goal

Build a small Rust greeter library, generate Swift bindings with
WeaveFFI, and call them from a SwiftUI iOS app running in the
simulator. A macOS command-line smoke test along the way proves the
Rust and the bindings agree before Xcode enters the picture.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Xcode 15 or later with the iOS SDK installed.
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- iOS Rust targets:

  ```bash
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```

## Step-by-step

### 1. Author the IDL

Save as `greeter.yml`:

```yaml
version: "0.9.0"
modules:
  - name: greeter
    errors:
      name: GreeterError
      codes:
        - { name: UnknownLang, code: 1, message: "unknown language" }
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
        throws: true
        params:
          - { name: name, type: string }
          - { name: lang, type: string }
        return: Greeting
```

`hello` can't fail, so it stays non-throwing. `greeting` declares
`throws: true` and reports codes from the module's `GreeterError`
domain when the language is unknown. Check it with
`weaveffi validate greeter.yml`; you should see `Validation passed`.

Put a `weaveffi.toml` beside it so the Swift package gets a stable name
(otherwise it defaults to the IDL's file stem, `greeter`):

```toml
[package]
name = "mygreeter"
version = "0.1.0"
```

### 2. Generate bindings

```bash
weaveffi generate greeter.yml -o generated
```

You should see, among other targets:

```text
generated/
├── c/
│   ├── weaveffi.c
│   └── weaveffi.h
└── swift/
    ├── Package.swift
    └── Sources/
        ├── CMygreeter/
        │   └── module.modulemap
        └── Mygreeter/
            └── Mygreeter.swift
```

The package name and both target names are the capitalized `[package]`
name: `CMygreeter` is the SwiftPM system library that wraps
`generated/c/weaveffi.h`, and `Mygreeter` is the Swift wrapper you
`import`. `Package.swift` declares iOS 13, macOS 10.15, tvOS 13, and
watchOS 6 as the minimum platforms.

### 3. Implement the Rust library

```bash
cargo init --lib mygreeter
```

`mygreeter/Cargo.toml`:

```toml
[package]
name = "mygreeter"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["staticlib", "cdylib"]

[dependencies]
weaveffi = "0.22"
```

The `staticlib` is what the iOS app links; the `cdylib` is for the macOS
smoke test in step 5. `mygreeter/src/lib.rs` is plain safe Rust. The macro
reads the annotated items and emits the `extern "C"` thunks the Swift
wrapper calls, so the module needs no `unsafe` and no hand-written
signatures:

```rust
#[weaveffi::module]
pub mod greeter {
    /// Codes the greeter reports from its throwing functions.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum GreeterError {
        /// unknown language
        UnknownLang = 1,
    }

    /// A greeting and the language it was rendered in.
    #[weaveffi::record]
    pub struct Greeting {
        pub message: String,
        pub lang: String,
    }

    /// Greet someone in English.
    #[weaveffi::export]
    pub fn hello(name: String) -> String {
        format!("Hello, {name}!")
    }

    /// Greet someone in the given language, failing on an unknown one.
    #[weaveffi::export]
    pub fn greeting(name: String, lang: String) -> Result<Greeting, GreeterError> {
        let message = match lang.as_str() {
            "en" => format!("Hello, {name}!"),
            "es" => format!("Hola, {name}!"),
            "fr" => format!("Bonjour, {name}!"),
            _ => return Err(GreeterError::UnknownLang),
        };
        Ok(Greeting { message, lang })
    }
}

weaveffi::export_runtime!();
```

`weaveffi extract mygreeter/src/lib.rs` produces the same IDL as
`greeter.yml` (plus the doc comments), so the two stay in step; you can
generate from either. The [Producer Macro](../guides/producer-macro.md)
guide covers every attribute.

### 4. Build for iOS targets

```bash
cargo build -p mygreeter --target aarch64-apple-ios --release
cargo build -p mygreeter --target aarch64-apple-ios-sim --release
cargo build -p mygreeter --target x86_64-apple-ios --release
```

The generated module map autolinks a library called `weaveffi`
(`link "weaveffi"`), so name the archives you hand to Xcode
`libweaveffi.a`. Combine the simulator architectures with `lipo` and
bundle everything in an `XCFramework` so Xcode can pick the right slice
automatically:

```bash
mkdir -p target/universal-ios-sim/release
lipo -create \
  target/aarch64-apple-ios-sim/release/libmygreeter.a \
  target/x86_64-apple-ios/release/libmygreeter.a \
  -output target/universal-ios-sim/release/libweaveffi.a
cp target/aarch64-apple-ios/release/libmygreeter.a \
  target/aarch64-apple-ios/release/libweaveffi.a

xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libweaveffi.a \
  -headers generated/c/ \
  -library target/universal-ios-sim/release/libweaveffi.a \
  -headers generated/c/ \
  -output MyGreeter.xcframework
```

### 5. Optional: smoke-test on macOS

Before opening Xcode, compile the generated wrapper together with a
`main.swift` against the host cdylib. Build it, give it the `libweaveffi`
name the module map links, and run:

```bash
cargo build -p mygreeter --release
ln -sf libmygreeter.dylib target/release/libweaveffi.dylib
```

`main.swift`:

```swift
print(Greeter.hello(name: "Swift"))
do {
    let g = try Greeter.greeting(name: "Ada", lang: "fr")
    print("\(g.message) (\(g.lang))")
    _ = try Greeter.greeting(name: "Ada", lang: "tlh")
} catch let e as GreeterError {
    print("greeting failed: \(e.localizedDescription) (code \(e.errorCode))")
}
```

```bash
swiftc \
  -I generated/swift/Sources/CMygreeter \
  -L target/release \
  -Xlinker -rpath -Xlinker target/release \
  generated/swift/Sources/Mygreeter/Mygreeter.swift main.swift -o greet
./greet
```

Expected output:

```text
Hello, Swift!
Bonjour, Ada! (fr)
greeting failed: unknown language (code 1)
```

### 6. Wire it into Xcode

1. Create a new iOS App in Xcode (SwiftUI or UIKit).
2. Drag `MyGreeter.xcframework` into the project navigator. Confirm it
   appears under **Build Phases > Link Binary With Libraries**.
3. **File > Add Package Dependencies > Add Local…** and pick
   `generated/swift/`. The package contributes the `CMygreeter` and
   `Mygreeter` targets; add the `Mygreeter` library product to your app.
4. **Build Settings > Header Search Paths**: add the path to
   `generated/c/` (e.g. `$(SRCROOT)/../generated/c`), which the module
   map's relative `header` path resolves against.
5. **Build Phases > Dependencies**: ensure `Mygreeter` is listed.

### 7. Call from Swift

```swift
import SwiftUI
import Mygreeter

struct ContentView: View {
    @State private var greeting = ""

    var body: some View {
        VStack {
            Text(greeting)
            Button("Greet") {
                greeting = Greeter.hello(name: "Swift")
            }
            Button("Greet in French") {
                do {
                    greeting = try Greeter.greeting(name: "Swift", lang: "fr").message
                } catch {
                    greeting = error.localizedDescription
                }
            }
        }
        .padding()
    }
}
```

The generated `Mygreeter` module exposes:

- `Greeter.hello(name:)`: non-throwing, returns `String`.
- `Greeter.greeting(name:lang:)`: declared `throws` in the IDL, so
  the Swift wrapper is `throws` and surfaces `GreeterError`; returns
  a `Greeting` value with `.message` and `.lang` properties.
- `GreeterError`: the module's error domain as a Swift `enum`
  conforming to `Error` and `LocalizedError`, one case per code
  (`.unknownLang(message:)`) plus an `errorCode` accessor. Runtime traps
  (a producer panic or a marshalling failure) arrive as the generic
  `WeaveFFIError.error(code:message:)` instead.
- `Greeting`: a plain Swift struct decoded from the value buffer the
  C ABI returns, with a public memberwise initializer.

## Verification

- Select an iOS Simulator target and press **Cmd+R**.
- Tap **Greet** in the running app; the label changes to
  `Hello, Swift!`. **Greet in French** changes it to `Bonjour, Swift!`.
- Re-run on a physical device after building for `aarch64-apple-ios`
  to confirm the device path also works.
- Common error mappings:

  | Symptom                                           | Likely cause                                                                 |
  |---------------------------------------------------|------------------------------------------------------------------------------|
  | `Undefined symbols for architecture arm64`        | Static library not linked, or it isn't named `libweaveffi.a`.                |
  | `Module 'CMygreeter' not found`                   | Header search path does not point at `generated/c/`.                         |
  | `No such module 'Mygreeter'`                      | Local Swift package not added under **Add Package Dependencies > Add Local…**.|
  | Crash when running on Intel simulator              | Build for `x86_64-apple-ios` and combine with `lipo`.                        |

## Cleanup

```bash
rm -rf generated/ MyGreeter.xcframework main.swift greet
rm -f target/release/libweaveffi.dylib   # the link alias from step 5
cargo clean -p mygreeter
```

Remove the `MyGreeter.xcframework` reference from the Xcode project
and undo the **Header Search Paths** edit.

## Next steps

- See the [Swift generator reference](../generators/swift.md) for the
  full type mapping.
- Read the [Memory Ownership](../guides/memory.md) guide to understand
  buffered value and interface lifetime rules.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Kotlin](kotlin.md) for a JVM target.
