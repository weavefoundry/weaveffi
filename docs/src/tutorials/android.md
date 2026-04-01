# Tutorial: Android App

This tutorial walks through building a Rust library, generating Kotlin
bindings with WeaveFFI, and integrating everything into an Android Studio
project.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel)
- Android Studio with NDK installed (via SDK Manager)
- WeaveFFI CLI installed (`cargo install weaveffi-cli`)
- Android Rust targets:

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
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

This produces (among other targets):

```text
generated/
├── c/
│   └── weaveffi.h
├── android/
│   ├── settings.gradle
│   ├── build.gradle
│   └── src/main/
│       ├── kotlin/com/weaveffi/WeaveFFI.kt
│       └── cpp/
│           ├── weaveffi_jni.c
│           └── CMakeLists.txt
└── scaffold.rs
```

## 3) Create the Rust library

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
crate-type = ["cdylib"]

[dependencies]
weaveffi-abi = { version = "0.1" }
```

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

Fill in the remaining functions using `scaffold.rs` as a guide.

## 4) Configure the Android NDK toolchain

Set the `ANDROID_NDK_HOME` environment variable to the NDK path. On
macOS with Android Studio's default install location:

```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/$(ls $HOME/Library/Android/sdk/ndk | sort -V | tail -1)"
```

Create a `.cargo/config.toml` in your project to point Cargo at the NDK
linkers:

```toml
[target.aarch64-linux-android]
linker = "aarch64-linux-android21-clang"

[target.armv7-linux-androideabi]
linker = "armv7a-linux-androideabi21-clang"

[target.x86_64-linux-android]
linker = "x86_64-linux-android21-clang"
```

Add the NDK toolchain to your `PATH`:

```bash
export PATH="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
```

Replace `darwin-x86_64` with `linux-x86_64` on Linux.

## 5) Cross-compile for Android

```bash
cargo build -p mygreeter --target aarch64-linux-android --release
cargo build -p mygreeter --target armv7-linux-androideabi --release
cargo build -p mygreeter --target x86_64-linux-android --release
```

This produces shared libraries:

```text
target/aarch64-linux-android/release/libmygreeter.so
target/armv7-linux-androideabi/release/libmygreeter.so
target/x86_64-linux-android/release/libmygreeter.so
```

## 6) Set up the Android Studio project

1. **Create a new Android project** in Android Studio (Empty Activity,
   Kotlin, minimum SDK 21+).

2. **Copy the generated android module.** Copy the `generated/android/`
   directory into your project as a Gradle module. In your root
   `settings.gradle`, add:

   ```groovy
   include ':weaveffi'
   project(':weaveffi').projectDir = new File('generated/android')
   ```

3. **Add the module dependency.** In your app's `build.gradle`:

   ```groovy
   dependencies {
       implementation project(':weaveffi')
   }
   ```

4. **Place the Rust shared libraries.** Copy each `.so` into the
   matching `jniLibs` directory:

   ```bash
   mkdir -p app/src/main/jniLibs/arm64-v8a
   mkdir -p app/src/main/jniLibs/armeabi-v7a
   mkdir -p app/src/main/jniLibs/x86_64

   cp target/aarch64-linux-android/release/libmygreeter.so \
     app/src/main/jniLibs/arm64-v8a/libmygreeter.so

   cp target/armv7-linux-androideabi/release/libmygreeter.so \
     app/src/main/jniLibs/armeabi-v7a/libmygreeter.so

   cp target/x86_64-linux-android/release/libmygreeter.so \
     app/src/main/jniLibs/x86_64/libmygreeter.so
   ```

5. **Copy the C header.** The JNI shims need `weaveffi.h`. Ensure the
   `CMakeLists.txt` in `generated/android/src/main/cpp/` has the
   correct `target_include_directories` pointing to `generated/c/`.

## 7) Call from Kotlin

```kotlin
import com.weaveffi.WeaveFFI

class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        val message = WeaveFFI.hello("Android")
        findViewById<TextView>(R.id.textView).text = message
    }
}
```

The generated `WeaveFFI` companion object loads the native library
automatically and exposes:

- `WeaveFFI.hello(name: String): String`
- `WeaveFFI.greeting(name: String, lang: String): Long` — returns an
  opaque handle to a `Greeting` struct

Struct wrappers (like `Greeting`) implement `Closeable` for
deterministic cleanup:

```kotlin
import com.weaveffi.Greeting

Greeting.create("Hi", "en").use { g ->
    println("${g.message} (${g.lang})")
}
```

## 8) Build and run

1. Sync Gradle in Android Studio.
2. Select an emulator or connected device.
3. Press **Run** (Shift+F10). The app should display "Hello, Android!".

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `UnsatisfiedLinkError: dlopen failed` | The `.so` is missing from `jniLibs/` or was built for the wrong ABI. |
| `java.lang.RuntimeException` from JNI | A WeaveFFI error was raised. Check the exception message for details. |
| Linker errors during `cargo build` | Ensure `ANDROID_NDK_HOME` is set and the NDK toolchain is on `PATH`. |
| `No implementation found for native method` | The JNI function names must match the Kotlin package path exactly. |

## Next steps

- See the [Android generator reference](../generators/android.md) for
  type mapping and JNI details.
- Read the [Error Handling](../guides/errors.md) guide — JNI shims
  convert C errors to `RuntimeException` automatically.
- Explore the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough.
