//! Content-hashing and per-generator caching for skip-if-unchanged builds.

use anyhow::{Context, Result};
use camino::Utf8Path;
use sha2::{Digest, Sha256};
use weaveffi_ir::ir::Api;

const CACHE_DIR: &str = ".weaveffi-cache";

/// Serialize the API to canonical JSON and return its SHA-256 hex digest.
///
/// The IR is first serialized to a `serde_json::Value`, whose `Object`
/// representation is backed by a `BTreeMap` (when the `preserve_order`
/// feature is not enabled). Re-serializing that `Value` therefore emits
/// keys in deterministic, lexicographic order regardless of the iteration
/// order of any source maps. This guarantees that two runs over the same
/// IR always produce the same hash.
pub fn hash_api(api: &Api) -> String {
    let value = serde_json::to_value(api).expect("Api serialization should not fail");
    let json = serde_json::to_string(&value).expect("Value serialization should not fail");
    let hash = Sha256::digest(json.as_bytes());
    format!("{hash:x}")
}

/// Return the SHA-256 hex digest of the API content keyed by `generator_name`.
///
/// Mixing the generator name into the digest gives every generator its own
/// cache key, so adding/removing a generator from the orchestrator does not
/// invalidate the others, and so two generators that happen to consume the
/// exact same IR still write to distinct cache entries.
pub fn hash_api_for_generator(api: &Api, generator_name: &str) -> String {
    let value = serde_json::to_value(api).expect("Api serialization should not fail");
    let json = serde_json::to_string(&value).expect("Value serialization should not fail");
    let mut hasher = Sha256::new();
    hasher.update(generator_name.as_bytes());
    hasher.update(b":");
    hasher.update(json.as_bytes());
    let hash = hasher.finalize();
    format!("{hash:x}")
}

/// Read the persisted hash for `generator_name` from `out_dir/.weaveffi-cache/`.
///
/// Returns `None` when no cache entry exists yet (or it is empty).
pub fn read_generator_cache(out_dir: &Utf8Path, generator_name: &str) -> Option<String> {
    let path = out_dir
        .join(CACHE_DIR)
        .join(format!("{generator_name}.hash"));
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist `hash` as the cache entry for `generator_name`.
///
/// Removes a stale legacy `.weaveffi-cache` regular file (written by older
/// CLI versions that used a single global cache) before creating the new
/// per-generator directory layout.
pub fn write_generator_cache(out_dir: &Utf8Path, generator_name: &str, hash: &str) -> Result<()> {
    let cache_dir = out_dir.join(CACHE_DIR);
    migrate_legacy_cache(out_dir)?;
    std::fs::create_dir_all(cache_dir.as_std_path())
        .with_context(|| format!("failed to create cache directory: {cache_dir}"))?;
    let path = cache_dir.join(format!("{generator_name}.hash"));
    std::fs::write(path.as_std_path(), hash)
        .with_context(|| format!("failed to write cache file: {path}"))?;
    Ok(())
}

/// Delete every persisted cache entry under `out_dir/.weaveffi-cache/`.
///
/// Called when `--force` is used so subsequent runs always regenerate.
pub fn invalidate_all(out_dir: &Utf8Path) -> Result<()> {
    let cache_dir = out_dir.join(CACHE_DIR);
    if cache_dir.is_dir() {
        std::fs::remove_dir_all(cache_dir.as_std_path())
            .with_context(|| format!("failed to remove cache directory: {cache_dir}"))?;
    } else if cache_dir.exists() {
        std::fs::remove_file(cache_dir.as_std_path())
            .with_context(|| format!("failed to remove legacy cache file: {cache_dir}"))?;
    }
    Ok(())
}

/// Remove a stale legacy single-file cache so we can create the new
/// per-generator directory in its place.
fn migrate_legacy_cache(out_dir: &Utf8Path) -> Result<()> {
    let cache_path = out_dir.join(CACHE_DIR);
    if cache_path.is_file() {
        std::fs::remove_file(cache_path.as_std_path())
            .with_context(|| format!("failed to remove legacy cache file: {cache_path}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{Generator, Orchestrator};
    use crate::config::GeneratorConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi_ir::ir::{Function, Module, Param, TypeRef};

    fn minimal_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        }
    }

    struct CountingGenerator {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl Generator for CountingGenerator {
        fn name(&self) -> &'static str {
            self.name
        }

        fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let dir = out_dir.join(self.name);
            std::fs::create_dir_all(dir.as_std_path())?;
            std::fs::write(dir.join("output.txt").as_std_path(), "generated")?;
            Ok(())
        }
    }

    #[test]
    fn hash_deterministic() {
        let api = minimal_api();
        let h1 = hash_api(&api);
        let h2 = hash_api(&api);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn hash_is_deterministic_across_runs() {
        let mut api = minimal_api();
        let mut generators = std::collections::BTreeMap::new();
        let mut swift = toml::value::Table::new();
        swift.insert(
            "module_name".into(),
            toml::Value::String("MySwiftModule".into()),
        );
        generators.insert("swift".into(), toml::Value::Table(swift));
        let mut android = toml::value::Table::new();
        android.insert(
            "package".into(),
            toml::Value::String("com.example.app".into()),
        );
        generators.insert("android".into(), toml::Value::Table(android));
        api.generators = Some(generators);

        let baseline = hash_api(&api);
        for _ in 0..100 {
            assert_eq!(
                hash_api(&api),
                baseline,
                "hash_api must produce identical output on every call"
            );
        }
    }

    #[test]
    fn hash_changes_on_modification() {
        let mut api = minimal_api();
        let h1 = hash_api(&api);

        api.modules[0].functions.push(Function {
            name: "subtract".to_string(),
            params: vec![
                Param {
                    name: "a".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        let h2 = hash_api(&api);

        assert_ne!(h1, h2);
    }

    #[test]
    fn per_generator_hash_includes_name() {
        let api = minimal_api();
        let h_c = hash_api_for_generator(&api, "c");
        let h_swift = hash_api_for_generator(&api, "swift");
        assert_ne!(h_c, h_swift);
        assert_eq!(h_c.len(), 64);
    }

    #[test]
    fn per_generator_hash_deterministic() {
        let api = minimal_api();
        assert_eq!(
            hash_api_for_generator(&api, "c"),
            hash_api_for_generator(&api, "c"),
        );
    }

    #[test]
    fn per_generator_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();

        let hash = hash_api_for_generator(&minimal_api(), "c");
        write_generator_cache(dir_path, "c", &hash).unwrap();

        let read_back = read_generator_cache(dir_path, "c");
        assert_eq!(read_back, Some(hash));
        assert_eq!(read_generator_cache(dir_path, "swift"), None);
    }

    #[test]
    fn read_generator_cache_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_generator_cache(dir_path, "c"), None);
    }

    #[test]
    fn invalidate_all_clears_cache() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        write_generator_cache(dir_path, "c", "abc").unwrap();
        write_generator_cache(dir_path, "swift", "def").unwrap();

        invalidate_all(dir_path).unwrap();
        assert_eq!(read_generator_cache(dir_path, "c"), None);
        assert_eq!(read_generator_cache(dir_path, "swift"), None);
    }

    #[test]
    fn legacy_cache_file_is_replaced_by_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(dir_path.join(CACHE_DIR), "stale-global-hash").unwrap();
        assert!(dir_path.join(CACHE_DIR).is_file());

        write_generator_cache(dir_path, "c", "fresh-hash").unwrap();

        assert!(dir_path.join(CACHE_DIR).is_dir());
        assert_eq!(
            read_generator_cache(dir_path, "c"),
            Some("fresh-hash".to_string())
        );
    }

    #[test]
    fn cache_file_written_after_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, false, None).unwrap();

        assert!(out_dir.join(CACHE_DIR).join("counting.hash").exists());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_prevents_regeneration() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second run should skip generation"
        );
    }

    #[test]
    fn cache_invalidated_on_api_change() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut modified_api = api;
        modified_api.modules[0].functions.push(Function {
            name: "subtract".to_string(),
            params: vec![
                Param {
                    name: "a".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });

        orch.run(&modified_api, out_dir, &config, false, None)
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "changed API should trigger regeneration"
        );
    }

    #[test]
    fn force_flag_bypasses_cache() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "force=true should bypass cache"
        );
    }

    #[test]
    fn legacy_cache_file_ignored_on_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(out_dir.join(CACHE_DIR), "stale-legacy").unwrap();

        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "legacy single-file cache must not skip first run"
        );
        assert!(out_dir.join(CACHE_DIR).is_dir());
    }

    #[test]
    fn single_generator_cache_invalidates_independently() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let config = GeneratorConfig::default();
        let c_calls = Arc::new(AtomicUsize::new(0));
        let s_calls = Arc::new(AtomicUsize::new(0));
        let c_gen = CountingGenerator {
            name: "c",
            calls: Arc::clone(&c_calls),
        };
        let s_gen = CountingGenerator {
            name: "swift",
            calls: Arc::clone(&s_calls),
        };
        let orch = Orchestrator::new()
            .with_generator(&c_gen)
            .with_generator(&s_gen);

        let api = minimal_api();
        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 1);
        assert_eq!(s_calls.load(Ordering::SeqCst), 1);

        // Invalidate only the C generator's cache; the API itself is unchanged.
        std::fs::remove_file(out_dir.join(CACHE_DIR).join("c.hash")).unwrap();

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(
            c_calls.load(Ordering::SeqCst),
            2,
            "C generator should re-run after its cache entry was removed"
        );
        assert_eq!(
            s_calls.load(Ordering::SeqCst),
            1,
            "Swift generator's cache is intact and must be skipped"
        );
    }
}
