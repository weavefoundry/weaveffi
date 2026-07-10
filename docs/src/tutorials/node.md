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
version: "0.5.0"
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
domain when the language is unknown.

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
│   ├── binding.gyp
│   ├── index.js
│   ├── package.json
│   ├── types.d.ts
│   └── weaveffi_addon.c
└── scaffold.rs
```

`weaveffi_addon.c` is a complete N-API addon that bridges Node's
runtime to the C ABI, and `binding.gyp` builds it with `node-gyp`; you
don't write any addon code yourself.

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
weaveffi-abi = { version = "0.14" }
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
    name: *const c_char,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    abi::error_set_ok(out_err);
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("world");
    let msg = format!("Hello, {name}!");
    CString::new(msg).unwrap().into_raw() as *const c_char
}

// Emit the WeaveFFI C ABI runtime symbols (free_string, free_bytes,
// error_clear, cancel_token_*), one line per cdylib.
abi::export_runtime!();
```

Use `scaffold.rs` for the rest of the API; it lists every symbol the
addon expects, with exact signatures.

### 4. Build the cdylib and the N-API addon

```bash
cargo build -p mygreeter --release
```

The generated `binding.gyp` links against `libweaveffi`, so give the
cdylib that name with a symlink, then build the addon in place
(`npm install` runs `node-gyp rebuild`; `LIBRARY_PATH` tells the
linker where to find the alias):

```bash
ln -sf libmygreeter.dylib target/release/libweaveffi.dylib   # .so on Linux
cd generated/node
LIBRARY_PATH="$PWD/../../target/release" npm install
```

The compiled addon lands at `build/Release/weaveffi.node`, which is
the first place the generated `index.js` looks. You can also point
the loader at any built addon with the `WEAVEFFI_ADDON` environment
variable, or ship a prebuilt binary as `index.node` next to
`index.js` (the fallback location).

### 5. Run the bindings locally

Save as `generated/node/demo.js`. Function names are camelCase with
the module prefix stripped, and the throwing `greeting` raises typed
error classes (`GreeterError` extends `WeaveFFIError`, with an
`UnknownLangError` subclass per code):

```javascript
const greeter = require("./index");

console.log(greeter.hello("Node"));

try {
  const g = greeter.greeting("Node", "en");
  console.log(`${g.message} (${g.lang})`);
} catch (e) {
  if (e instanceof greeter.GreeterError) {
    console.log(`${e.name}: ${e.errorMessage}`);
  } else {
    throw e;
  }
}
```

Run it (the cdylib must be on the loader path so the addon's
`libweaveffi` reference resolves):

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
import * as greeter from "./index";

const msg: string = greeter.hello("TypeScript");
const g: greeter.Greeting = greeter.greeting("TS", "en");
console.log(`${g.message} (${g.lang})`);
```

### 6. Prepare for publishing

Copy the built addon to the fallback location the loader checks, so
consumers don't need node-gyp:

```bash
cp build/Release/weaveffi.node index.node
```

Then edit `generated/node/package.json`:

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
  | `Error: Cannot find module './index.node'`               | The addon isn't built; run `npm install` or set `WEAVEFFI_ADDON`.              |
  | `Error: dlopen ... not found`                            | Cdylib not on the loader path; set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.    |
  | `TypeError: greeter.hello is not a function`             | The addon is stale; rerun `npm install` after IDL edits.                       |
  | Crashes on `require()`                                   | Addon built for the wrong Node.js version or architecture; rebuild.            |

## Cleanup

```bash
rm -rf generated/
cargo clean -p mygreeter
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
