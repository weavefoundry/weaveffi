//! Per-target generation records for skip-if-unchanged builds and stale-file
//! cleanup.
//!
//! After generating a target, the orchestrator writes
//! `{out_dir}/.weaveffi-cache/{target}.json`: a hash of every input that
//! affects the target's output (the canonical IR, the library identity, the
//! target name, its serialized config, and the CLI version) plus the path and
//! content hash of every file it wrote. A later run skips the target only
//! when the input hash matches *and* every recorded file is still on disk
//! unmodified, so deleting or hand-editing a generated file regenerates it.
//! Files recorded by the previous run that the current run no longer
//! produces are removed, so a renamed type never leaves a stale file behind,
//! while files the user added (a `node_modules/`, a build directory) are
//! never touched.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use weaveffi_model::ir::Api;
use weaveffi_model::resolved::ResolvedApi;

const CACHE_DIR: &str = ".weaveffi-cache";

/// Version string folded into every input hash, so upgrading the CLI
/// regenerates everything.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What one generation of one target produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// [`hash_generator_inputs`] for the run that produced the files.
    pub inputs: String,
    /// Every file written, relative to the output directory (with `/`
    /// separators), mapped to the SHA-256 of its contents.
    pub files: BTreeMap<String, String>,
}

impl Record {
    /// Whether every recorded file still exists under `out_dir` with the
    /// recorded contents.
    #[must_use]
    pub fn files_intact(&self, out_dir: &Utf8Path) -> bool {
        self.files.iter().all(|(rel, hash)| {
            std::fs::read(out_dir.join(rel).as_std_path())
                .is_ok_and(|bytes| &hash_bytes(&bytes) == hash)
        })
    }
}

/// The SHA-256 hex digest of `bytes`.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

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
/// Panics if the API cannot be serialized to JSON, which does not happen for
/// well-formed inputs.
#[must_use]
pub fn hash_api(api: &Api) -> String {
    hash_bytes(canonical_json(api).as_bytes())
}

/// The SHA-256 hex digest of every input that affects a single target's
/// output: the canonical IR, the library identity, the target's name, the
/// target's serialized config, and the CLI version.
///
/// # Panics
///
/// Panics if the API or identity cannot be serialized to JSON, which does not
/// happen for well-formed inputs.
#[must_use]
pub fn hash_generator_inputs(api: &ResolvedApi, target: &str, config_bytes: &[u8]) -> String {
    let identity =
        serde_json::to_string(api.identity()).expect("identity serialization should not fail");
    let mut hasher = Sha256::new();
    for part in [
        b"v3".as_slice(),
        CLI_VERSION.as_bytes(),
        target.as_bytes(),
        canonical_json(api.api()).as_bytes(),
        identity.as_bytes(),
        config_bytes,
    ] {
        hasher.update(part);
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

fn record_path(out_dir: &Utf8Path, target: &str) -> Utf8PathBuf {
    out_dir.join(CACHE_DIR).join(format!("{target}.json"))
}

/// Read the record of the last generation of `target` under `out_dir`, if
/// any (a missing or unreadable record means "never generated").
#[must_use]
pub fn read_record(out_dir: &Utf8Path, target: &str) -> Option<Record> {
    let text = std::fs::read_to_string(record_path(out_dir, target)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist the record of a generation of `target`.
///
/// # Errors
///
/// Returns an error when the cache directory cannot be created or the record
/// cannot be written.
///
/// # Panics
///
/// Panics if the record cannot be serialized, which does not happen.
pub fn write_record(out_dir: &Utf8Path, target: &str, record: &Record) -> Result<()> {
    let path = record_path(out_dir, target);
    let dir = out_dir.join(CACHE_DIR);
    std::fs::create_dir_all(dir.as_std_path())
        .with_context(|| format!("failed to create cache directory: {dir}"))?;
    let json = serde_json::to_string_pretty(record).expect("record serializes");
    std::fs::write(path.as_std_path(), json)
        .with_context(|| format!("failed to write cache file: {path}"))
}

/// Forget every target's record, so the next run regenerates everything.
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
    use weaveffi_model::ir::{Module, CURRENT_SCHEMA_VERSION};

    fn api(name: &str) -> Api {
        Api {
            version: CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: name.into(),
                doc: None,
                functions: vec![],
                interfaces: vec![],
                callback_interfaces: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
        }
    }

    #[test]
    fn input_hash_covers_api_target_config_and_identity() {
        let r = ResolvedApi::assume_valid(api("math"));
        let base = hash_generator_inputs(&r, "swift", b"{}");
        assert_eq!(base, hash_generator_inputs(&r, "swift", b"{}"));
        assert_ne!(base, hash_generator_inputs(&r, "c", b"{}"));
        assert_ne!(base, hash_generator_inputs(&r, "swift", b"{\"x\":1}"));
        assert_ne!(
            base,
            hash_generator_inputs(&ResolvedApi::assume_valid(api("other")), "swift", b"{}")
        );
        let renamed = r
            .clone()
            .with_identity(weaveffi_model::pkg::Identity::named("kv"));
        assert_ne!(base, hash_generator_inputs(&renamed, "swift", b"{}"));
        assert_eq!(hash_api(&api("math")), hash_api(&api("math")));
    }

    #[test]
    fn records_round_trip_and_detect_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_record(out, "swift"), None);
        std::fs::create_dir_all(out.join("swift")).unwrap();
        std::fs::write(out.join("swift/a.swift"), "a").unwrap();
        let record = Record {
            inputs: "abc".into(),
            files: [("swift/a.swift".to_string(), hash_bytes(b"a"))].into(),
        };
        write_record(out, "swift", &record).unwrap();
        let back = read_record(out, "swift").unwrap();
        assert_eq!(back, record);
        assert!(back.files_intact(out));
        std::fs::write(out.join("swift/a.swift"), "edited").unwrap();
        assert!(!back.files_intact(out));
        std::fs::remove_file(out.join("swift/a.swift")).unwrap();
        assert!(!back.files_intact(out));
        invalidate_all(out).unwrap();
        assert_eq!(read_record(out, "swift"), None);
        invalidate_all(out).unwrap();
    }
}
