//! Packaging manifests: `pyproject.toml` and `README.md` for the generated
//! tree, plus the `setup.py` and README of each per-platform wheel tree the
//! `package` command assembles.

use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

/// Escape a string for a TOML basic string.
fn toml_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render the `pyproject.toml`: a PEP 621 project built with setuptools,
/// shipping the stub, the `py.typed` marker, and any bundled library as
/// package data.
pub(crate) fn render_pyproject_toml(
    id: &Identity,
    dist: &str,
    import: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pyproject.toml");
    let version = &id.version;
    let description = toml_str(&id.description_or_default());
    let mut extra = String::new();
    if let Some(license) = &id.license {
        extra.push_str(&format!(
            "license = {{ text = \"{}\" }}\n",
            toml_str(license)
        ));
    }
    if !id.authors.is_empty() {
        let authors = id
            .authors
            .iter()
            .map(|a| format!("{{ name = \"{}\" }}", toml_str(a)))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("authors = [{authors}]\n"));
    }
    let mut urls = String::new();
    if let Some(homepage) = &id.homepage {
        urls.push_str(&format!("Homepage = \"{}\"\n", toml_str(homepage)));
    }
    if let Some(repository) = &id.repository {
        urls.push_str(&format!("Repository = \"{}\"\n", toml_str(repository)));
    }
    if !urls.is_empty() {
        urls = format!("\n[project.urls]\n{urls}");
    }
    format!(
        r#"{prelude}[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"

[project]
name = "{dist}"
version = "{version}"
description = "{description}"
requires-python = ">=3.9"
{extra}{urls}
[tool.setuptools]
packages = ["{import}"]

[tool.setuptools.package-data]
"{import}" = ["py.typed", "*.pyi", "*.so", "*.dylib", "*.dll"]

{trailer}"#,
    )
}

/// Render the PEP 561 `py.typed` marker. Type checkers only look for the
/// file's presence (its content is ignored unless it says `partial`), so it
/// carries the standard prelude like every other generated file.
pub(crate) fn render_py_typed(input_basename: &str) -> String {
    render_prelude(CommentStyle::Hash, input_basename)
}

/// Render the `README.md` for the generated tree.
pub(crate) fn render_readme(
    id: &Identity,
    dist: &str,
    import: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let (darwin, linux, windows) = id.library_files();
    let env = id.library_env_var();
    format!(
        r#"{prelude}# {dist} (Python)

Python bindings for the `{library}` native library, built on `ctypes`.

## Requirements

- Python 3.9 or later
- The native library (`{linux}`, `{darwin}`, or `{windows}`)

## Install

```bash
pip install .
```

## Loading the native library

The bindings load the library from `{env}` when it is set (a full path),
then from inside the installed package, then from the system loader's search
path.

## Usage

```python
import {import}
```

{trailer}"#,
        library = id.library,
    )
}

/// Render a `setup.py` for a packaged wheel: it ships the bundled library as
/// package data and forces a non-pure (platform-tagged) wheel.
pub(crate) fn render_packaged_setup_py(
    dist: &str,
    version: &str,
    import: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "setup.py");
    format!(
        r#"{prelude}from setuptools import setup
from setuptools.dist import Distribution


class _BinaryDistribution(Distribution):
    # Force a non-pure, platform-tagged wheel: the package bundles a native
    # shared library, so it is not portable across platforms.
    def has_ext_modules(self):
        return True


setup(
    name="{dist}",
    version="{version}",
    packages=["{import}"],
    package_data={{"{import}": ["py.typed", "*.pyi", "*.so", "*.dylib", "*.dll"]}},
    include_package_data=True,
    distclass=_BinaryDistribution,
)

{trailer}"#,
    )
}

/// README for a packaged per-platform Python wheel tree. `tag` is the
/// platform's wheel tag (`platform.python_platform_tag()`), which the caller
/// has already established exists.
pub(crate) fn render_packaged_readme(
    dist: &str,
    import: &str,
    platform: crate::platform::Platform,
    tag: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    format!(
        r#"{prelude}# {dist} (Python, {plat})

Python bindings with the native library bundled for `{plat}`. The library
loads automatically; no external setup is required.

## Build the wheel

```bash
python -m build --wheel
```

Tag the resulting wheel for this platform with `{tag}` (for example via
`wheel tags --platform-tag {tag} dist/*.whl`) before publishing.

## Usage

```python
import {import}
```

{trailer}"#,
        plat = platform.id(),
    )
}
