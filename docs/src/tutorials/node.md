# Node.js npm Package

## Goal

Build a small Rust greeter library, generate Node.js bindings with
WeaveFFI, build the N-API addon, and call the bindings from a
JavaScript script. By the end you will have an `npm`-installable
package shape ready to publish.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Node.js 16 or later and `npm`.
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- A C compiler in the `PATH` (Xcode CLT on macOS, `build-essential` on
  Linux, MSVC build tools on Windows) for the N-API addon build.

## Step-by-step

### 1. Author the IDL

Save as `greeter.yml`:

```yaml
version: "0.3.0"
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

### 2. Generate bindings

```bash
weaveffi generate greeter.yml -o generated --scaffold
```

Among other targets you should see:

```text
generated/
├── c/
│   └── weaveffi.h
├── node/
│   ├── index.js
│   ├── types.d.ts
│   └── package.json
└── scaffold.rs
```

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
crate-type = ["cdylib"]

[dependencies]
weaveffi-abi = { version = "0.1" }
```

`mygreeter/src/lib.rs`:

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
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) { abi::free_string(ptr); }

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) { abi::free_bytes(ptr, len); }

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) { abi::error_clear(err); }
```

Use `scaffold.rs` for the rest of the API. You also need an N-API
addon crate that bridges Node's runtime to the C ABI — see
`samples/node-addon` in the WeaveFFI repository for a working example
to copy.

### 4. Build the cdylib and the N-API addon

```bash
cargo build -p mygreeter --release
cargo build -p node-addon --release
```

Copy the addon into the generated package as `index.node`:

macOS:

```bash
cp target/release/libindex.dylib generated/node/index.node
```

Linux:

```bash
cp target/release/libindex.so generated/node/index.node
```

Windows:

```powershell
copy target\release\index.dll generated\node\index.node
```

### 5. Run the bindings locally

Save as `generated/node/demo.js`:

```javascript
const weaveffi = require("./index");

const msg = weaveffi.hello("Node");
console.log(msg);
```

Run it (the cdylib must be on the loader path):

macOS:

```bash
cd generated/node
DYLD_LIBRARY_PATH=../../target/release node demo.js
```

Linux:

```bash
cd generated/node
LD_LIBRARY_PATH=../../target/release node demo.js
```

For TypeScript consumers, the generated `types.d.ts` is enough:

```typescript
import * as weaveffi from "./index";

const msg: string = weaveffi.hello("TypeScript");
const g: weaveffi.Greeting = weaveffi.greeting("TS", "en");
console.log(`${g.message} (${g.lang})`);
```

### 6. Prepare for publishing

Edit `generated/node/package.json`:

```json
{
  "name": "@myorg/greeter",
  "version": "0.1.0",
  "main": "index.js",
  "types": "types.d.ts",
  "files": [
    "index.js",
    "index.node",
    "types.d.ts"
  ],
  "os": ["darwin", "linux"],
  "cpu": ["x64", "arm64"]
}
```

`files` must include `index.node`. For multi-platform packages,
publish per-platform optional dependencies (e.g.
`@myorg/greeter-darwin-arm64`) and use an install script to pick the
right binary.

### 7. Publish

```bash
cd generated/node
npm pack
npm publish
```

For scoped packages, append `--access public`. Consumers then run:

```bash
npm install @myorg/greeter
```

```javascript
const { hello } = require("@myorg/greeter");
console.log(hello("npm"));
```

## Verification

- `node demo.js` prints `Hello, Node!` and exits with code `0`.
- `npm pack` produces a `.tgz` containing `index.node`,
  `types.d.ts`, and `index.js`.
- TypeScript consumers see the `Greeting` interface and `hello`
  signature without manual type declarations.
- Common error mappings:

  | Symptom                                                  | Likely cause                                                                  |
  |----------------------------------------------------------|-------------------------------------------------------------------------------|
  | `Error: Cannot find module './index.node'`               | The compiled addon is missing; copy the platform-specific binary in.           |
  | `Error: dlopen ... not found`                            | Cdylib not on the loader path; set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.    |
  | `TypeError: weaveffi.hello is not a function`            | The N-API addon did not export the expected symbols; rebuild after IDL edits.  |
  | Crashes on `require()`                                   | Addon built for the wrong Node.js version or architecture; rebuild.            |

## Cleanup

```bash
rm -rf generated/
cargo clean -p mygreeter
cargo clean -p node-addon
```

If you published a test version, mark it as deprecated with
`npm deprecate @myorg/greeter@0.1.0 "test publish"`.

## Next steps

- See the [Node generator reference](../generators/node.md) for the
  full type mapping and `types.d.ts` layout.
- Read [Memory Ownership](../guides/memory.md) for struct lifecycle
  semantics.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Python](python.md) for a sibling
  scripting target.
