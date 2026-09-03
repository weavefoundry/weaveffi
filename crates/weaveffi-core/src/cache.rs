//! Content-hashing and per-target caching for skip-if-unchanged builds.
//!
//! The orchestrator stores one hash per target under
//! `{out_dir}/.weaveffi-cache/{target}.hash`. The hash covers every input
//! that affects that target's output (the canonical IR, the target name, its
//! serialized config, and the CLI version), so a change to any of them
//! re-runs exactly the targets it affects.

use anyhow::{Context, Result};
use camino::Utf8Path;
use sha2::{Digest, Sha256};
use weaveffi_ir::ir::Api;

use crate::resolved::ResolvedApi;

const CACHE_DIR: &str = ".weaveffi-cache";

/// Version string baked into every cache entry. Bumping the WeaveFFI CLI
/// version automatically invalidates every cache file so users never see
/// stale generator output after an upgrade.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Serialize `api` to canonical JSON (keys in lexicographic order, so two
/// runs over the same IR always agree).
fn canonical_json(api: &Api) -> String {
    let value = serde_json::to_value(api).expect("Api serialization should not fail");
    serde_json::to_string(&value).expect("Value serialization should not fail")
}

/// The SHA-256 hex digest of the API's canonical JSON.
///
/// # Panics
///
/// Panics if `api` cannot be serialized to JSON. This does not happen for a
/// well-formed [`Api`], whose IR is plain serializable data.
pub fn hash_api(api: &Api) -> String {
    format!("{:x}", Sha256::digest(canonical_json(api).as_bytes()))
}

/// The SHA-256 hex digest of every input that affects a single target's
/// output: the canonical IR, the attached package identity, the target's
/// name, the target's serialized config (canonical JSON bytes from
/// [`Target::config_hash_input`](crate::codegen::Target::config_hash_input)),
/// and the CLI version.
///
/// # Panics
///
/// Panics if the API or package cannot be serialized to JSON, which does not
/// happen for well-formed inputs.
pub fn hash_generator_inputs(api: &ResolvedApi, target: &str, config_bytes: &[u8]) -> String {
    let package =
        serde_json::to_string(&api.package()).expect("Package serialization should not fail");
    let mut hasher = Sha256::new();
    hasher.update(b"v2\0");
    hasher.update(CLI_VERSION.as_bytes());
    hasher.update(b"\0");
    hasher.update(target.as_bytes());
    hasher.update(b"\0");
    hasher.update(canonical_json(api.api()).as_bytes());
    hasher.update(b"\0");
    hasher.update(package.as_bytes());
    hasher.update(b"\0");
    hasher.update(config_bytes);
    format!("{:x}", hasher.finalize())
}

fn entry_path(out_dir: &Utf8Path, target: &str) -> camino::Utf8PathBuf {
    out_dir.join(CACHE_DIR).join(format!("{target}.hash"))
}

/// Read the persisted hash for `target`, or `None` when no non-empty entry
/// exists yet.
pub fn read_generator_cache(out_dir: &Utf8Path, target: &str) -> Option<String> {
    std::fs::read_to_string(entry_path(out_dir, target))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist `hash` as the cache entry for `target`.
///
/// # Errors
///
/// Returns an error if the cache directory cannot be created or the hash
/// file cannot be written.
pub fn write_generator_cache(out_dir: &Utf8Path, target: &str, hash: &str) -> Result<()> {
    let dir = out_dir.join(CACHE_DIR);
    let path = dir.join(format!("{target}.hash"));
    std::fs::create_dir_all(dir.as_std_path())
        .with_context(|| format!("failed to create cache directory: {dir}"))?;
    std::fs::write(path.as_std_path(), hash)
        .with_context(|| format!("failed to write cache file: {path}"))?;
    Ok(())
}

/// Delete every persisted cache entry under `out_dir/.weaveffi-cache/`.
/// Called when `--force` is used so subsequent runs always regenerate.
///
/// # Errors
///
/// Returns an error if the cache directory exists but cannot be removed.
pub fn invalidate_all(out_dir: &Utf8Path) -> Result<()> {
    let cache_dir = out_dir.join(CACHE_DIR);
    if cache_dir.exists() {
        std::fs::remove_dir_all(cache_dir.as_std_path())
            .with_context(|| format!("failed to remove cache directory: {cache_dir}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Module, CURRENT_SCHEMA_VERSION};

    fn api(name: &str) -> Api {
        Api {
            version: CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: name.into(),
                doc: None,
                functions: vec![],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }
    }

    #[test]
    fn hashes_are_stable_and_keyed_on_every_input() {
        let a = api("math");
        assert_eq!(hash_api(&a), hash_api(&a.clone()));
        assert_ne!(hash_api(&a), hash_api(&api("math2")));
        let r = ResolvedApi::assume_valid(a);
        let base = hash_generator_inputs(&r, "swift", b"{}");
        assert_eq!(base, hash_generator_inputs(&r, "swift", b"{}"));
        assert_ne!(base, hash_generator_inputs(&r, "c", b"{}"));
        assert_ne!(base, hash_generator_inputs(&r, "swift", b"{\"x\":1}"));
        assert_ne!(
            base,
            hash_generator_inputs(&ResolvedApi::assume_valid(api("other")), "swift", b"{}")
        );
        let with_pkg = r.clone().with_package(crate::pkg::Package {
            name: Some("kv".into()),
            ..Default::default()
        });
        assert_ne!(base, hash_generator_inputs(&with_pkg, "swift", b"{}"));
    }

    #[test]
    fn entries_round_trip_and_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_generator_cache(out, "swift"), None);
        write_generator_cache(out, "swift", "abc").unwrap();
        write_generator_cache(out, "c", "def").unwrap();
        assert_eq!(read_generator_cache(out, "swift").as_deref(), Some("abc"));
        assert_eq!(read_generator_cache(out, "c").as_deref(), Some("def"));
        invalidate_all(out).unwrap();
        assert_eq!(read_generator_cache(out, "swift"), None);
        invalidate_all(out).unwrap();
    }
}
