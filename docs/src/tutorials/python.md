# Python Package

## Goal

Build a small Rust greeter library, generate Python ctypes bindings
with WeaveFFI, install the package locally, and call it from a Python
script.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Python 3.8 or later (`python3 --version`).
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- `pip` (ships with Python).

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

Among other targets, you should see:

```text
generated/
├── c/
│   └── weaveffi.h
├── python/
│   ├── pyproject.toml
│   ├── setup.py
│   ├── README.md
│   └── greeter/
│       ├── __init__.py
│       ├── weaveffi.py
│       └── weaveffi.pyi
└── scaffold.rs
```

The package directory and distribution name follow the IDL package name
(here `greeter`). The Python target uses ctypes: no native extension to
compile on the Python side.

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
bindings expect, with exact signatures.

### 4. Build the cdylib

```bash
cargo build -p mygreeter --release
```

Produces:

| Platform | Output                                  |
|----------|-----------------------------------------|
| macOS    | `target/release/libmygreeter.dylib`     |
| Linux    | `target/release/libmygreeter.so`        |
| Windows  | `target/release/mygreeter.dll`          |

### 5. Install the Python package

```bash
cd generated/python
pip install .
```

Use `pip install -e .` for an editable install during development.

### 6. Make the cdylib findable

The simplest option on any platform is the `WEAVEFFI_LIBRARY`
environment variable, which the generated loader checks first and
treats as an explicit path:

```bash
WEAVEFFI_LIBRARY=target/release/libmygreeter.dylib python demo.py
```

Without the override, the loader looks for `libweaveffi.dylib` (macOS),
`libweaveffi.so` (Linux), or `weaveffi.dll` (Windows) on the system
loader path. Symlink or copy your cdylib to the expected name and set
the loader path.

macOS:

```bash
cp target/release/libmygreeter.dylib target/release/libweaveffi.dylib
DYLD_LIBRARY_PATH=target/release python demo.py
```

Linux:

```bash
cp target/release/libmygreeter.so target/release/libweaveffi.so
LD_LIBRARY_PATH=target/release python demo.py
```

Windows: place `weaveffi.dll` next to your script or add its
directory to `PATH`.

### 7. Use the bindings

Save as `demo.py`. Function names are snake_case with the module
prefix stripped, and the throwing `greeting` raises the typed
exception hierarchy (`GreeterError` extends `WeaveFFIError`, with an
`UnknownLang` subclass per code):

```python
from greeter import hello, greeting, GreeterError

print(hello("Python"))

try:
    g = greeting("Python", "en")
    print(f"{g.message} ({g.lang})")
except GreeterError as e:
    print(f"Error {e.code}: {e.message}")
```

Struct wrappers free the Rust allocation when garbage-collected; for
deterministic cleanup, `del g` after you are done with the object.

## Verification

- `pip show greeter` lists the package.
- Running `demo.py` prints `Hello, Python!` and `Hi (en)` (or whatever
  `Greeting` you constructed).
- `mypy demo.py` reports no errors thanks to the generated
  `weaveffi.pyi` stub.
- Common error mappings:

  | Symptom                                                   | Likely cause                                                                  |
  |-----------------------------------------------------------|-------------------------------------------------------------------------------|
  | `OSError: dlopen ... not found`                           | Cdylib not on the loader path; set `WEAVEFFI_LIBRARY` or the loader path.      |
  | `GreeterError: ...` at runtime                             | Rust reported a domain error code; inspect `e.code` and `e.message`.          |
  | `ModuleNotFoundError: No module named 'greeter'`           | Package not installed; rerun `pip install .` from `generated/python/`.        |
  | mypy complains about `greeter`                            | Make sure `weaveffi.pyi` ships next to `weaveffi.py` in the package.          |

## Cleanup

```bash
pip uninstall greeter
rm -rf generated/
cargo clean -p mygreeter
```

## Next steps

- See the [Python generator reference](../generators/python.md) for
  the full type mapping and memory contract.
- Read [Error Handling](../guides/errors.md) for the cross-target
  error model.
- Try the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough or [Node.js](node.md) for a sibling
  scripting target.
