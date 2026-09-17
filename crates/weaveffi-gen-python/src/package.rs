//! Packaging manifests: `pyproject.toml`, `setup.py`, and `README.md` for
//! both the plain generate tree and the per-platform packaged wheel trees.
//! All of these are TOML, Python, or Markdown, so none route through the
//! shared JSON manifest builder.

use weaveffi_core::pkg::ResolvedPackage;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

/// Render the `pyproject.toml` for the generated package.
pub(crate) fn render_pyproject_toml(
    package: &ResolvedPackage,
    import_name: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pyproject.toml");
    let name = &package.name;
    let version = &package.version;
    let description = package.description_or_default();
    let mut extra = String::new();
    if let Some(license) = &package.license {
        extra.push_str(&format!("license = {{ text = \"{license}\" }}\n"));
    }
    if !package.authors.is_empty() {
        let authors = package
            .authors
            .iter()
            .map(|a| format!("{{ name = \"{a}\" }}"))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("authors = [{authors}]\n"));
    }
    if let Some(homepage) = &package.homepage {
        extra.push_str(&format!("[project.urls]\nHomepage = \"{homepage}\"\n"));
    } else if let Some(repository) = &package.repository {
        extra.push_str(&format!("[project.urls]\nRepository = \"{repository}\"\n"));
    }
    format!(
        r#"{prelude}[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"

[project]
name = "{name}"
version = "{version}"
description = "{description}"
requires-python = ">=3.8"
{extra}
[tool.setuptools]
packages = ["{import_name}"]

[tool.setuptools.package-data]
"{import_name}" = ["py.typed", "*.pyi", "*.so", "*.dylib", "*.dll"]

{trailer}"#,
    )
}

/// Render the PEP 561 `py.typed` marker. Type checkers only look for the
/// file's presence (its content is ignored unless it says `partial`), so it
/// carries the standard prelude like every other generated file.
pub(crate) fn render_py_typed(input_basename: &str) -> String {
    render_prelude(CommentStyle::Hash, input_basename)
}

/// Render the plain `setup.py` for a generated (non-packaged) tree.
pub(crate) fn render_setup_py(
    package: &ResolvedPackage,
    import_name: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "setup.py");
    let name = &package.name;
    let version = &package.version;
    format!(
        r#"{prelude}from setuptools import setup

setup(
    name="{name}",
    version="{version}",
    packages=["{import_name}"],
)

{trailer}"#,
    )
}

/// Render the `README.md` for a generated (non-packaged) tree.
pub(crate) fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let import_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Python)

Auto-generated Python bindings using ctypes.

## Prerequisites

- Python >= 3.8
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
pip install .
```

## Development install

```bash
pip install -e .
```

## Usage

```python
from {import_name} import *
```

{trailer}"#
    )
}

/// Render a `setup.py` for a packaged wheel: it ships the bundled library as
/// package data and forces a non-pure (platform-tagged) wheel.
pub(crate) fn render_packaged_setup_py(
    package: &ResolvedPackage,
    import_name: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "setup.py");
    let name = &package.name;
    let version = &package.version;
    format!(
        r#"{prelude}from setuptools import setup
from setuptools.dist import Distribution


class _BinaryDistribution(Distribution):
    # Force a non-pure, platform-tagged wheel: the package bundles a native
    # shared library, so it is not portable across platforms.
    def has_ext_modules(self):
        return True


setup(
    name="{name}",
    version="{version}",
    packages=["{import_name}"],
    package_data={{"{import_name}": ["py.typed", "*.pyi", "*.so", "*.dylib", "*.dll"]}},
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
    package: &ResolvedPackage,
    import_name: &str,
    platform: weaveffi_core::platform::Platform,
    tag: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    format!(
        r#"{prelude}# {name} (Python, {plat})

Auto-generated Python bindings with the native library bundled for `{plat}`.
The library loads automatically; no external setup is required.

## Build the wheel

```bash
python -m build --wheel
```

Tag the resulting wheel for this platform with `{tag}` (for example via
`wheel tags --platform-tag {tag} dist/*.whl`) before publishing.

## Usage

```python
from {import_name} import *
```

{trailer}"#,
        plat = platform.id(),
    )
}
