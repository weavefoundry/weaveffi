# Python Package

## Goal

Build a small Rust greeter library, generate Python ctypes bindings
with WeaveFFI, install the package locally, and call it from a Python
script.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel).
- Python 3.7 or later (`python3 --version`).
- WeaveFFI CLI (`cargo install weaveffi-cli`).
- `pip` (ships with Python).

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

Among other targets, you should see:

```text
generated/
├── c/
│   └── weaveffi.h
├── python/
│   ├── pyproject.toml
│   ├── setup.py
│   ├── README.md
│   └── weaveffi/
│       ├── __init__.py
│       ├── weaveffi.py
│       └── weaveffi.pyi
└── scaffold.rs
```

The Python target uses ctypes — no native extension to compile on the
Python side.

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

Use `scaffold.rs` for the rest of the API.

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

The generated loader looks for `libweaveffi.dylib` (macOS),
`libweaveffi.so` (Linux), or `weaveffi.dll` (Windows). Symlink or copy
your cdylib to the expected name and set the loader path.

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
directory to `PATH`. For production, copy the cdylib into the package
directory and update `weaveffi.py`'s loader path.

### 7. Use the bindings

Save as `demo.py`:

```python
from weaveffi import hello, greeting, WeaveffiError

print(hello("Python"))

try:
    g = greeting("Python", "en")
    print(f"{g.message} ({g.lang})")
except WeaveffiError as e:
    print(f"Error {e.code}: {e.message}")
```

Struct wrappers free the Rust allocation when garbage-collected; for
deterministic cleanup, `del g` after you are done with the object.

## Verification

- `pip show weaveffi` lists the package.
- Running `demo.py` prints `Hello, Python!` and `Hi (en)` (or whatever
  `Greeting` you constructed).
- `mypy demo.py` reports no errors thanks to the generated
  `weaveffi.pyi` stub.
- Common error mappings:

  | Symptom                                                   | Likely cause                                                                  |
  |-----------------------------------------------------------|-------------------------------------------------------------------------------|
  | `OSError: dlopen ... not found`                           | Cdylib not on the loader path; set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.    |
  | `WeaveffiError: ...` at runtime                            | Rust returned a non-zero error code; inspect `e.code` and `e.message`.        |
  | `ModuleNotFoundError: No module named 'weaveffi'`          | Package not installed; rerun `pip install .` from `generated/python/`.        |
  | mypy complains about `weaveffi`                           | Make sure `weaveffi.pyi` ships next to `weaveffi.py` in the package.          |

## Cleanup

```bash
pip uninstall weaveffi
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
