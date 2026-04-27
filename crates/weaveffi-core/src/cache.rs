//! Content-hashing and caching for skip-if-unchanged builds.

use anyhow::Result;
use camino::Utf8Path;
use sha2::{Digest, Sha256};
use weaveffi_ir::ir::Api;

const CACHE_FILE: &str = ".weaveffi-cache";

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

/// Read a previously written cache hash from the output directory.
pub fn read_cache(out_dir: &Utf8Path) -> Option<String> {
    std::fs::read_to_string(out_dir.join(CACHE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write the hash to the cache file in the output directory.
pub fn write_cache(out_dir: &Utf8Path, hash: &str) -> Result<()> {
    std::fs::write(out_dir.join(CACHE_FILE), hash)?;
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
        calls: Arc<AtomicUsize>,
    }

    impl Generator for CountingGenerator {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(out_dir.join("output.txt").as_std_path(), "generated")?;
            Ok(())
        }
    }

    #[test]
    fn hash_deterministic() {
        let api = minimal_api();
        let h1 = hash_api(&api);
        let h2 = hash_api(&api);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex digest
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
    fn cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();

        let hash = hash_api(&minimal_api());
        write_cache(dir_path, &hash).unwrap();

        let read_back = read_cache(dir_path);
        assert_eq!(read_back, Some(hash));
    }

    #[test]
    fn read_cache_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_cache(dir_path), None);
    }

    #[test]
    fn cache_file_written_after_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = minimal_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, false, None).unwrap();

        assert!(out_dir.join(CACHE_FILE).exists());
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
}
