# Tutorial: Node.js npm Package

This tutorial walks through building a Rust library, generating Node.js
N-API bindings with WeaveFFI, and publishing as an npm package.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel)
- Node.js 16+ and npm
- WeaveFFI CLI installed (`cargo install weaveffi-cli`)

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
├── node/
│   ├── index.js
│   ├── types.d.ts
│   └── package.json
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

You also need an N-API addon crate that bridges Node's JavaScript
runtime to the C ABI. See `samples/node-addon` in the WeaveFFI
repository for a working example.

## 4) Build the N-API addon

Build the Rust library:

```bash
cargo build -p mygreeter --release
```

Build the N-API addon (which links against your library and the C ABI):

```bash
cargo build -p node-addon --release
```

Copy the compiled addon into the generated node package:

**macOS:**

```bash
cp target/release/libindex.dylib generated/node/index.node
```

**Linux:**

```bash
cp target/release/libindex.so generated/node/index.node
```

The file must be named `index.node` — the generated `index.js` loader
requires it at that path.

## 5) Test locally

Create a test script `demo.js` in the `generated/node/` directory:

```javascript
const weaveffi = require("./index");

const msg = weaveffi.hello("Node");
console.log(msg); // "Hello, Node!"
```

Run it:

**macOS:**

```bash
cd generated/node
DYLD_LIBRARY_PATH=../../target/release node demo.js
```

**Linux:**

```bash
cd generated/node
LD_LIBRARY_PATH=../../target/release node demo.js
```

### TypeScript support

The generated `types.d.ts` provides full type definitions. In a
TypeScript project:

```typescript
import * as weaveffi from "./index";

const msg: string = weaveffi.hello("TypeScript");
console.log(msg);

const g: weaveffi.Greeting = weaveffi.greeting("TS", "en");
console.log(`${g.message} (${g.lang})`);
```

## 6) Prepare for npm publish

Edit `generated/node/package.json` to set your package metadata:

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

Key points:

- **`files`** must include `index.node` (the compiled N-API addon).
- **`os`** and **`cpu`** fields document supported platforms.
- For cross-platform packages, consider publishing platform-specific
  optional dependencies (e.g.
  `@myorg/greeter-darwin-arm64`) and using an install script to select
  the right binary.

## 7) Publish

```bash
cd generated/node
npm pack    # creates a .tgz for inspection
npm publish # publishes to the npm registry
```

For scoped packages, use `npm publish --access public`.

### Consuming the published package

```bash
npm install @myorg/greeter
```

```javascript
const { hello } = require("@myorg/greeter");
console.log(hello("npm")); // "Hello, npm!"
```

## Shipping prebuilt binaries (optional)

Compiling an N-API addon from source requires a C toolchain on every
consumer's machine, which is painful for CI runners and end-users who
install via `npm install` without any build infrastructure. The
generated `package.json` is pre-wired with hooks for the two most
common prebuilt-binary tools so you can publish ready-to-load `.node`
files alongside the source.

### The `binary` block

The generated `package.json` ships with a `node-pre-gyp`-compatible
`binary` descriptor:

```json
"binary": {
  "module_name": "weaveffi",
  "module_path": "./build/Release/",
  "remote_path": "./{module_name}/v{version}/{configuration}/",
  "package_name": "{module_name}-v{version}-{node_abi}-{platform}-{arch}.tar.gz",
  "host": "https://example.com/weaveffi-prebuilds/"
}
```

Point `host` at the bucket, GitHub Releases URL, or CDN where you
publish prebuilt tarballs. The `{node_abi}`, `{platform}`, and
`{arch}` placeholders are expanded by `node-pre-gyp` at install time
so each consumer downloads the artifact for its runtime.

### Option A: `prebuildify`

[`prebuildify`](https://github.com/prebuild/prebuildify) bundles
prebuilt binaries directly into the npm tarball under `prebuilds/`,
so consumers never hit the network. The generated `package.json`
already lists `prebuilds/` in `files` and exposes a `prebuild`
script:

```bash
npm install --save-dev prebuildify node-gyp-build
npm run prebuild    # writes prebuilds/<platform>-<arch>/weaveffi.node
```

Then swap the native loader in a small wrapper (or `index.js`) to
`node-gyp-build`, which picks the matching prebuild or falls back to
a source build when none is available.

### Option B: `@mapbox/node-pre-gyp`

[`@mapbox/node-pre-gyp`](https://github.com/mapbox/node-pre-gyp)
downloads prebuilts on demand using the `binary` block above. Build
and stage each target with the generated `package` script:

```bash
npm install --save-dev @mapbox/node-pre-gyp
npm run package     # writes build/stage/<remote_path>/<package_name>
```

Upload the resulting tarballs to the configured `host`, then change
the `install` script to
`node-pre-gyp install --fallback-to-build` so consumers fetch the
prebuilt artifact and only compile from source when no match exists.

### Picking a tool

- Choose **`prebuildify`** for small support matrices and simpler
  publishing — every binary ships inside the npm tarball.
- Choose **`node-pre-gyp`** for larger matrices (many Node ABIs /
  platforms / arches) where you prefer hosting binaries separately
  and downloading them on demand.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `Error: Cannot find module './index.node'` | The compiled addon is missing. Copy the built `.dylib`/`.so` as `index.node`. |
| `Error: dlopen ... not found` | The Rust shared library is not on the library path. Set `DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`. |
| `TypeError: weaveffi.hello is not a function` | The N-API addon did not export the expected symbols. Check that the addon registers all functions. |
| Crashes on `require()` | The addon was built for a different Node.js version or architecture. Rebuild with the correct target. |

## Next steps

- See the [Node generator reference](../generators/node.md) for type
  mapping details and the full `types.d.ts` format.
- Read the [Memory Ownership](../guides/memory.md) guide for struct
  lifecycle semantics.
- Explore the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough.
