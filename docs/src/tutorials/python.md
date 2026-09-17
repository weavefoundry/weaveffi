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

Put a `weaveffi.toml` beside it so the Python distribution and import
package get a stable name (otherwise both default to the IDL's file stem,
`greeter`):

```toml
[package]
name = "mygreeter"
version = "0.1.0"
```

### 2. Generate bindings

```bash
weaveffi generate greeter.yml -o generated
```

Among other targets, you should see:

```text
generated/
├── c/
│   ├── weaveffi.c
│   └── weaveffi.h
└── python/
    ├── pyproject.toml
    ├── setup.py
    ├── README.md
    └── mygreeter/
        ├── __init__.py
        ├── weaveffi.py
        └── weaveffi.pyi
```

The package directory and distribution name follow the `[package]` name
from `weaveffi.toml`. The Python target uses ctypes: no native extension to
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
weaveffi = "0.22"
```

`mygreeter/src/lib.rs` is plain safe Rust. The macro reads the annotated
items and emits the `extern "C"` thunks the ctypes loader binds to, so the
module needs no `unsafe` and no hand-written signatures:

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
ln -sf libmygreeter.dylib target/release/libweaveffi.dylib
DYLD_LIBRARY_PATH=target/release python demo.py
```

Linux:

```bash
ln -sf libmygreeter.so target/release/libweaveffi.so
LD_LIBRARY_PATH=target/release python demo.py
```

Windows: place `weaveffi.dll` next to your script or add its
directory to `PATH`.

### 7. Use the bindings

Save as `demo.py`. Function names are snake_case with the module
prefix stripped, and the throwing `greeting` raises the typed
exception hierarchy (`GreeterError` extends `WeaveFFIError`, with an
`UnknownLang` subclass per code, also reachable as
`GreeterError.UnknownLang`):

```python
from mygreeter import hello, greeting, GreeterError

print(hello("Python"))

try:
    g = greeting("Python", "en")
    print(f"{g.message} ({g.lang})")
    greeting("Python", "tlh")
except GreeterError as e:
    print(f"Error {e.code}: {e.message}")
```

`Greeting` is a plain Python dataclass decoded from the value buffer
the C ABI returns; there's no native allocation to manage.

## Verification

- `pip show mygreeter` lists the package.
- Running `demo.py` prints:

  ```text
  Hello, Python!
  Hello, Python! (en)
  Error 1: unknown language
  ```

- `mypy demo.py` reports no errors thanks to the generated `weaveffi.pyi`
  stub, which the package ships alongside a PEP 561 `py.typed` marker so the
  installed wheel is typed too.
- Common error mappings:

  | Symptom                                                   | Likely cause                                                                  |
  |-----------------------------------------------------------|-------------------------------------------------------------------------------|
  | `OSError: dlopen ... not found`                           | Cdylib not on the loader path; set `WEAVEFFI_LIBRARY` or the loader path.      |
  | `mygreeter.weaveffi.UnknownLang: (1) unknown language`     | Rust reported a domain error code; inspect `e.code` and `e.message`.          |
  | `WeaveFFIError` with a negative code                       | A producer panic (-2) or marshalling failure (-3); see [Error Handling](../guides/errors.md). |
  | `ModuleNotFoundError: No module named 'mygreeter'`         | Package not installed; rerun `pip install .` from `generated/python/`.        |
  | mypy says the module is untyped                           | Reinstall from a fresh `generated/python/` so `py.typed` and `weaveffi.pyi` ship. |

## Cleanup

```bash
pip uninstall mygreeter
rm -rf generated/
rm -f target/release/libweaveffi.dylib   # the link alias from step 6
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
