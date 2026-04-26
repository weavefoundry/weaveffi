# Tutorial: Python Package

This tutorial walks through building a Rust library, generating Python
ctypes bindings with WeaveFFI, and packaging it for `pip install`.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable channel)
- Python 3.7+
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

The generated Python package uses `ctypes` — no native extension
compilation is needed on the Python side.

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

## 4) Build the shared library

```bash
cargo build -p mygreeter --release
```

This produces the shared library:

| Platform | Output |
|----------|--------|
| macOS | `target/release/libmygreeter.dylib` |
| Linux | `target/release/libmygreeter.so` |
| Windows | `target/release/mygreeter.dll` |

## 5) Install the Python package

```bash
cd generated/python
pip install .
```

For development iteration, use an editable install:

```bash
pip install -e .
```

## 6) Make the shared library findable

The generated ctypes loader looks for `libweaveffi.dylib` (macOS),
`libweaveffi.so` (Linux), or `weaveffi.dll` (Windows) on the system
library search path.

Rename or symlink your library to match the expected name, then set the
library path:

**macOS:**

```bash
cp target/release/libmygreeter.dylib target/release/libweaveffi.dylib
DYLD_LIBRARY_PATH=target/release python your_script.py
```

**Linux:**

```bash
cp target/release/libmygreeter.so target/release/libweaveffi.so
LD_LIBRARY_PATH=target/release python your_script.py
```

**Windows:**

Place `weaveffi.dll` in the same directory as your script or add its
directory to `PATH`.

Alternatively, for production, copy the shared library into the Python
package directory and adjust the loader path in `weaveffi.py`.

## 7) Use the bindings

Create a script called `demo.py`:

```python
from weaveffi import hello, greeting

msg = hello("Python")
print(msg)  # "Hello, Python!"

g = greeting("Python", "en")
print(f"{g.message} ({g.lang})")
```

Run it:

```bash
DYLD_LIBRARY_PATH=target/release python demo.py   # macOS
LD_LIBRARY_PATH=target/release python demo.py      # Linux
```

### Error handling

WeaveFFI errors are raised as `WeaveffiError` exceptions:

```python
from weaveffi import WeaveffiError

try:
    result = hello("")
except WeaveffiError as e:
    print(f"Error {e.code}: {e.message}")
```

### Struct lifecycle

Struct wrappers automatically free Rust memory when garbage collected.
For explicit control, delete the reference:

```python
g = greeting("Python", "en")
print(g.message)
del g  # immediately calls the Rust destroy function
```

## 8) Type stubs and IDE support

The generated `weaveffi.pyi` stub file provides type information for
editors and `mypy`:

```bash
mypy demo.py
```

IDEs like VS Code and PyCharm will show autocomplete for all generated
functions, classes, and properties.

## 9) Build wheels for PyPI with cibuildwheel

The generated `pyproject.toml` ships with a `[tool.cibuildwheel]` block
so you can produce Linux, macOS, and Windows wheels from a single
`cibuildwheel` invocation and upload them to PyPI.

The defaults look like:

```toml
[tool.cibuildwheel]
build = "cp38-* cp39-* cp310-* cp311-* cp312-*"
skip = "*-musllinux_* pp*"
before-build = "weaveffi build ../../api.yml"

[tool.cibuildwheel.linux]
archs = ["x86_64", "aarch64"]

[tool.cibuildwheel.macos]
archs = ["x86_64", "arm64"]

[tool.cibuildwheel.windows]
archs = ["AMD64"]
```

`before-build` regenerates bindings and compiles a fresh cdylib in each
per-platform build environment. Adjust the IDL path (`../../api.yml`) to
match your layout if you do not generate into `generated/python/`.

### Local dry run

Install the tool once and build the matrix locally:

```bash
pipx install cibuildwheel
cd generated/python
pipx run cibuildwheel --platform linux    # or macos / windows
```

Wheels land in `./wheelhouse/`.

### GitHub Actions recipe

Drop this into `.github/workflows/wheels.yml` to build the full matrix
on every tag push and attach the wheels as an artifact:

```yaml
name: wheels

on:
  push:
    tags: ["v*"]

jobs:
  build_wheels:
    name: ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-13, macos-14, windows-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install weaveffi-cli
      - name: Build wheels
        uses: pypa/cibuildwheel@v2
        with:
          package-dir: generated/python
      - uses: actions/upload-artifact@v4
        with:
          name: wheels-${{ matrix.os }}
          path: ./wheelhouse/*.whl
```

### Upload to PyPI

Once the matrix succeeds, publish with `twine`:

```bash
pipx run twine upload wheelhouse/*.whl
```

For trusted publishing set up a PyPI project and use the
`pypa/gh-action-pypi-publish` action in the same workflow.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `OSError: dlopen ... not found` | The shared library is not on the library search path. Set `DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`. |
| `WeaveffiError` at runtime | A Rust-side error was returned. Check the error code and message. |
| `ModuleNotFoundError: No module named 'weaveffi'` | Run `pip install .` from `generated/python/`. |
| mypy type errors | Ensure `weaveffi.pyi` is in the package directory alongside `weaveffi.py`. |

## Next steps

- See the [Python generator reference](../generators/python.md) for
  type mapping and memory management details.
- Read the [Error Handling](../guides/errors.md) guide for the full
  error model.
- Explore the [Calculator tutorial](calculator.md) for a simpler
  end-to-end walkthrough.
