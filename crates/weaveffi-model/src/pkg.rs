//! Package identity: the one source of every name a generated package uses.
//!
//! The `[package]` table of a project's `weaveffi.toml` ([`Package`]) declares
//! what the library is called and how it is published. The CLI resolves it
//! once into an [`Identity`], and validation builds it into the
//! [`Model`](crate::model::Model), so the C symbol prefix, the native library
//! name, and every ecosystem's package, module, and namespace name derive
//! from the same values. Nothing a generator emits is named after WeaveFFI
//! itself, so any number of WeaveFFI-built libraries can coexist in one
//! process and one application.

use serde::{Deserialize, Serialize};

/// Fallback package version when the project omits `package.version`.
pub const DEFAULT_VERSION: &str = "0.1.0";

/// Fallback package name for an API that was never given an identity (unit
/// tests and other in-memory uses). The CLI always derives a real one.
pub const DEFAULT_NAME: &str = "api";

/// The `[package]` table of `weaveffi.toml`.
///
/// Every field is optional in the file; the CLI fills the gaps from the
/// producer's `Cargo.toml` (for a Rust producer) or the input file name (for
/// an IDL) before calling [`Identity::new`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Package {
    /// Distribution name used by every ecosystem (npm, PyPI, NuGet, and so
    /// on) unless a target's own configuration overrides it.
    pub name: Option<String>,
    /// Semantic version stamped into every manifest.
    pub version: Option<String>,
    /// Short package description.
    pub description: Option<String>,
    /// License identifier, typically an SPDX expression.
    pub license: Option<String>,
    /// Package authors.
    pub authors: Vec<String>,
    /// Project homepage URL.
    pub homepage: Option<String>,
    /// Source repository URL.
    pub repository: Option<String>,
    /// C symbol prefix for an IDL-defined library. Defaults to the snake-case
    /// package name. A Rust producer's prefix is always its crate's library
    /// name, so setting this for a Rust producer is an error.
    pub c_prefix: Option<String>,
    /// Base name of the native library (`lib{library}.so`, `{library}.dll`)
    /// for an IDL-defined library. Defaults to the snake-case package name.
    /// A Rust producer's library is always its crate's library name.
    pub library: Option<String>,
}

/// The resolved identity of the library being bound.
///
/// Every field is final: generators read names from here (and from the
/// target's own configuration overrides) and never invent their own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Identity {
    /// Distribution name as published (may contain `-` or `.`).
    pub name: String,
    /// C symbol prefix: a lower-snake C identifier that begins every exported
    /// symbol and every C type name (`{prefix}_error`, `{prefix}_kv_get`).
    pub prefix: String,
    /// Native library base name: the producer is `lib{library}.so`,
    /// `lib{library}.dylib`, or `{library}.dll`.
    pub library: String,
    /// Semantic version.
    pub version: String,
    /// Short description, if declared.
    pub description: Option<String>,
    /// License identifier, if declared.
    pub license: Option<String>,
    /// Package authors.
    pub authors: Vec<String>,
    /// Project homepage URL, if declared.
    pub homepage: Option<String>,
    /// Source repository URL, if declared.
    pub repository: Option<String>,
}

impl Default for Identity {
    fn default() -> Self {
        Self::named(DEFAULT_NAME)
    }
}

impl Identity {
    /// An identity with only a name: the prefix and library are the
    /// snake-case name and every metadata field is unset.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self::new(name, &Package::default())
    }

    /// Resolve `package` (whose own `name`, if set, wins over `name`) into a
    /// complete identity. The prefix and library default to the snake-case
    /// form of the resolved name.
    #[must_use]
    pub fn new(name: &str, package: &Package) -> Self {
        let name = non_empty(package.name.as_ref()).unwrap_or_else(|| name.trim().to_string());
        let name = if name.is_empty() {
            DEFAULT_NAME.to_string()
        } else {
            name
        };
        let snake = c_ident(&name);
        Self {
            prefix: non_empty(package.c_prefix.as_ref())
                .map(|p| c_ident(&p))
                .unwrap_or_else(|| snake.clone()),
            library: non_empty(package.library.as_ref()).unwrap_or(snake),
            version: non_empty(package.version.as_ref())
                .unwrap_or_else(|| DEFAULT_VERSION.to_string()),
            description: non_empty(package.description.as_ref()),
            license: non_empty(package.license.as_ref()),
            authors: package.authors.clone(),
            homepage: non_empty(package.homepage.as_ref()),
            repository: non_empty(package.repository.as_ref()),
            name,
        }
    }

    /// The uppercase prefix used for C preprocessor macros (`KVSTORE_API`,
    /// `KVSTORE_ABI_VERSION`).
    #[must_use]
    pub fn macro_prefix(&self) -> String {
        self.prefix.to_ascii_uppercase()
    }

    /// The name as a lower-snake identifier (a Python import package, a Ruby
    /// `require` path, a Dart package). `"my-kv.store"` becomes
    /// `"my_kv_store"`.
    #[must_use]
    pub fn snake_name(&self) -> String {
        c_ident(&self.name)
    }

    /// The name as an UpperCamelCase identifier (a Swift module, a .NET
    /// namespace, a Ruby module). `"my-kv.store"` becomes `"MyKvStore"`.
    #[must_use]
    pub fn pascal_name(&self) -> String {
        pascal_ident(&self.name)
    }

    /// The description, or a generated one when unset.
    #[must_use]
    pub fn description_or_default(&self) -> String {
        self.description
            .clone()
            .unwrap_or_else(|| format!("Native bindings for {}", self.name))
    }

    /// The environment variable a generated loader consults for an explicit
    /// library path (`KVSTORE_LIBRARY`).
    #[must_use]
    pub fn library_env_var(&self) -> String {
        format!("{}_LIBRARY", self.macro_prefix())
    }

    /// Platform file names for the native library: `(macOS, Linux, Windows)`.
    #[must_use]
    pub fn library_files(&self) -> (String, String, String) {
        let l = &self.library;
        (
            format!("lib{l}.dylib"),
            format!("lib{l}.so"),
            format!("{l}.dll"),
        )
    }
}

/// UpperCamelCase identifier form of an arbitrary name. Word boundaries are
/// runs of non-alphanumerics; existing camel humps are preserved.
/// `"my-kv.store"` becomes `"MyKvStore"`.
#[must_use]
pub fn pascal_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut start_word = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if start_word {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
            start_word = false;
        } else {
            start_word = true;
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'N');
    }
    if out.is_empty() {
        pascal_ident(DEFAULT_NAME)
    } else {
        out
    }
}

/// Lower-snake C identifier form of an arbitrary name: non-alphanumerics
/// collapse to `_`, and a leading digit gets a `lib_` prefix.
/// `"my-kv.store"` becomes `"my_kv_store"`.
#[must_use]
pub fn c_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us && !out.is_empty() {
            out.push('_');
            prev_us = true;
        }
    }
    let trimmed = out.trim_end_matches('_');
    if trimmed.is_empty() {
        DEFAULT_NAME.to_string()
    } else if trimmed.starts_with(|c: char| c.is_ascii_digit()) {
        format!("lib_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// The file stem of a path-like input name: `"path/to/kv.store.yml"` becomes
/// `"kv"`.
#[must_use]
pub fn name_from_basename(basename: &str) -> String {
    let file = basename.rsplit(['/', '\\']).next().unwrap_or(basename);
    file.split('.').next().unwrap_or(file).to_string()
}

fn non_empty(s: Option<&String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_drives_prefix_and_library() {
        let id = Identity::named("my-kv.store");
        assert_eq!(id.name, "my-kv.store");
        assert_eq!(id.prefix, "my_kv_store");
        assert_eq!(id.library, "my_kv_store");
        assert_eq!(id.macro_prefix(), "MY_KV_STORE");
        assert_eq!(id.pascal_name(), "MyKvStore");
        assert_eq!(id.library_env_var(), "MY_KV_STORE_LIBRARY");
        assert_eq!(id.version, DEFAULT_VERSION);
    }

    #[test]
    fn package_table_wins() {
        let pkg = Package {
            name: Some("kvstore".into()),
            version: Some("1.2.0".into()),
            c_prefix: Some("kv".into()),
            library: Some("kvstore_core".into()),
            ..Package::default()
        };
        let id = Identity::new("ignored", &pkg);
        assert_eq!(id.name, "kvstore");
        assert_eq!(id.prefix, "kv");
        assert_eq!(id.library, "kvstore_core");
        assert_eq!(id.version, "1.2.0");
        assert_eq!(id.description_or_default(), "Native bindings for kvstore");
    }

    #[test]
    fn identifiers_sanitize() {
        assert_eq!(c_ident("Kvstore"), "kvstore");
        assert_eq!(c_ident("--"), DEFAULT_NAME);
        assert_eq!(c_ident("3d-engine"), "lib_3d_engine");
        assert_eq!(pascal_ident("kvstore"), "Kvstore");
        assert_eq!(pascal_ident("3d"), "N3d");
        assert_eq!(name_from_basename("path/to/contacts.yml"), "contacts");
    }

    #[test]
    fn package_table_parses_from_toml() {
        let pkg: Package =
            toml::from_str("name = \"kv\"\nversion = \"2.0.0\"\nauthors = [\"A\", \"B\"]\n")
                .unwrap();
        assert_eq!(pkg.name.as_deref(), Some("kv"));
        assert_eq!(pkg.authors.len(), 2);
        assert!(toml::from_str::<Package>("bogus = 1").is_err());
    }
}
