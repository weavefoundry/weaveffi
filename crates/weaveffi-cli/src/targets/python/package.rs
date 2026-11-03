//! Packaging manifests: `pyproject.toml` and `README.md` for the generated
//! tree, plus the long description of each wheel the `package` command
//! writes.

use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

/// Escape a string for a TOML basic string.
fn toml_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render the `pyproject.toml`: a PEP 621 project built with setuptools,
/// shipping the `py.typed` marker and any bundled library as
/// package data.
pub(crate) fn render_pyproject_toml(
    id: &Identity,
    dist: &str,
    import: &str,
    requires_python: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash);
    let trailer = render_trailer(CommentStyle::Hash, "pyproject.toml");
    let version = &id.version;
    let description = toml_str(&id.description_or_default());
    let requires_python = toml_str(requires_python);
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
requires-python = "{requires_python}"
{extra}{urls}
[tool.setuptools]
packages = ["{import}"]

[tool.setuptools.package-data]
"{import}" = ["py.typed", "*.so", "*.dylib", "*.dll"]

{trailer}"#,
    )
}

/// Render the PEP 561 `py.typed` marker. Type checkers only look for the
/// file's presence (its content is ignored unless it says `partial`), so it
/// carries the standard prelude like every other generated file.
pub(crate) fn render_py_typed() -> String {
    render_prelude(CommentStyle::Hash)
}

/// Render the `README.md` for the generated tree.
pub(crate) fn render_readme(id: &Identity, dist: &str, import: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let (darwin, linux, windows) = id.library_files();
    let env = id.library_env_var();
    format!(
        r#"{prelude}# {dist} (Python)

Python bindings for the `{library}` native library, built on `ctypes`.

## Requirements

- Python 3.10 or later
- The native library (`{linux}`, `{darwin}`, or `{windows}`)

## Install

```bash
pip install .
```

## Loading the native library

The bindings load the library from `{env}` when it's set (a full path),
then from inside the installed package, then from the system loader's search
path. Importing the package checks that the library is the one the bindings
were generated for; when it can't be loaded or doesn't match, the import
raises `LibraryLoadError`, an `ImportError`.

## Usage

```python
import {import}
```

{trailer}"#,
        library = id.library,
    )
}

/// The long description of a packaged wheel: the bundled library loads
/// automatically, so it only says how to import the package.
pub(crate) fn render_wheel_readme(id: &Identity, dist: &str, import: &str) -> String {
    format!(
        "# {dist}\n\n{}\n\nPython bindings for the `{library}` native library, which this \
         wheel bundles.\n\n```python\nimport {import}\n```\n",
        id.description_or_default(),
        library = id.library,
    )
}
