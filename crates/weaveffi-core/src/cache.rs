//! Content-hashing and caching for skip-if-unchanged builds.

use anyhow::Result;
use camino::Utf8Path;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, ListenerDef, Module,
    Param, StructDef, StructField, TypeRef,
};

const CACHE_FILE: &str = ".weaveffi-cache";

/// Walk the `Api` and emit a deterministic byte representation:
/// map keys are sorted lexicographically, struct fields are in alphabetical
/// order, and floats are formatted with fixed precision (`"{:.17}"`).
///
/// The output is compact JSON backed by [`serde_json::Map`] (a `BTreeMap`),
/// so iteration order is fully determined by key ordering.
pub fn canonical_serialize(api: &Api) -> String {
    let value = api_to_value(api);
    serde_json::to_string(&value).expect("canonical JSON serialization should not fail")
}

/// Serialize the API canonically and return its SHA-256 hex digest.
pub fn hash_api(api: &Api) -> String {
    let canonical = canonical_serialize(api);
    let hash = Sha256::digest(canonical.as_bytes());
    format!("{hash:x}")
}

fn float_str(f: f64) -> String {
    format!("{:.17}", f)
}

fn obj(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

fn opt_string(s: Option<&String>) -> Value {
    s.map(|s| Value::String(s.clone())).unwrap_or(Value::Null)
}

fn api_to_value(api: &Api) -> Value {
    obj([
        (
            "generators",
            api.generators
                .as_ref()
                .map(|g| {
                    let mut m = Map::new();
                    for (k, v) in g {
                        m.insert(k.clone(), toml_to_value(v));
                    }
                    Value::Object(m)
                })
                .unwrap_or(Value::Null),
        ),
        (
            "modules",
            Value::Array(api.modules.iter().map(module_to_value).collect()),
        ),
        ("version", Value::String(api.version.clone())),
    ])
}

fn module_to_value(m: &Module) -> Value {
    obj([
        (
            "callbacks",
            Value::Array(m.callbacks.iter().map(callback_to_value).collect()),
        ),
        (
            "enums",
            Value::Array(m.enums.iter().map(enum_to_value).collect()),
        ),
        (
            "errors",
            m.errors
                .as_ref()
                .map(error_domain_to_value)
                .unwrap_or(Value::Null),
        ),
        (
            "functions",
            Value::Array(m.functions.iter().map(function_to_value).collect()),
        ),
        (
            "listeners",
            Value::Array(m.listeners.iter().map(listener_to_value).collect()),
        ),
        (
            "modules",
            Value::Array(m.modules.iter().map(module_to_value).collect()),
        ),
        ("name", Value::String(m.name.clone())),
        (
            "structs",
            Value::Array(m.structs.iter().map(struct_def_to_value).collect()),
        ),
    ])
}

fn function_to_value(f: &Function) -> Value {
    obj([
        ("async", Value::Bool(f.r#async)),
        ("cancellable", Value::Bool(f.cancellable)),
        ("deprecated", opt_string(f.deprecated.as_ref())),
        ("doc", opt_string(f.doc.as_ref())),
        ("name", Value::String(f.name.clone())),
        (
            "params",
            Value::Array(f.params.iter().map(param_to_value).collect()),
        ),
        (
            "returns",
            f.returns
                .as_ref()
                .map(type_ref_to_value)
                .unwrap_or(Value::Null),
        ),
        ("since", opt_string(f.since.as_ref())),
    ])
}

fn param_to_value(p: &Param) -> Value {
    obj([
        ("mutable", Value::Bool(p.mutable)),
        ("name", Value::String(p.name.clone())),
        ("type", type_ref_to_value(&p.ty)),
    ])
}

fn type_ref_to_value(ty: &TypeRef) -> Value {
    serde_json::to_value(ty).expect("TypeRef serializes to a JSON string")
}

fn callback_to_value(cb: &CallbackDef) -> Value {
    obj([
        ("doc", opt_string(cb.doc.as_ref())),
        ("name", Value::String(cb.name.clone())),
        (
            "params",
            Value::Array(cb.params.iter().map(param_to_value).collect()),
        ),
        (
            "returns",
            cb.returns
                .as_ref()
                .map(type_ref_to_value)
                .unwrap_or(Value::Null),
        ),
    ])
}

fn listener_to_value(l: &ListenerDef) -> Value {
    obj([
        ("doc", opt_string(l.doc.as_ref())),
        ("event_callback", Value::String(l.event_callback.clone())),
        ("name", Value::String(l.name.clone())),
    ])
}

fn enum_to_value(e: &EnumDef) -> Value {
    obj([
        ("doc", opt_string(e.doc.as_ref())),
        ("name", Value::String(e.name.clone())),
        (
            "variants",
            Value::Array(e.variants.iter().map(enum_variant_to_value).collect()),
        ),
    ])
}

fn enum_variant_to_value(v: &EnumVariant) -> Value {
    obj([
        ("doc", opt_string(v.doc.as_ref())),
        ("name", Value::String(v.name.clone())),
        ("value", Value::Number(v.value.into())),
    ])
}

fn struct_def_to_value(s: &StructDef) -> Value {
    obj([
        ("builder", Value::Bool(s.builder)),
        ("doc", opt_string(s.doc.as_ref())),
        (
            "fields",
            Value::Array(s.fields.iter().map(struct_field_to_value).collect()),
        ),
        ("name", Value::String(s.name.clone())),
    ])
}

fn struct_field_to_value(f: &StructField) -> Value {
    obj([
        (
            "default",
            f.default.as_ref().map(yaml_to_value).unwrap_or(Value::Null),
        ),
        ("doc", opt_string(f.doc.as_ref())),
        ("name", Value::String(f.name.clone())),
        ("type", type_ref_to_value(&f.ty)),
    ])
}

fn error_domain_to_value(e: &ErrorDomain) -> Value {
    obj([
        (
            "codes",
            Value::Array(e.codes.iter().map(error_code_to_value).collect()),
        ),
        ("name", Value::String(e.name.clone())),
    ])
}

fn error_code_to_value(c: &ErrorCode) -> Value {
    obj([
        ("code", Value::Number(c.code.into())),
        ("message", Value::String(c.message.clone())),
        ("name", Value::String(c.name.clone())),
    ])
}

fn toml_to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => Value::String(float_str(*f)),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_value).collect()),
        toml::Value::Table(t) => {
            let mut m = Map::new();
            for (k, v) in t {
                m.insert(k.clone(), toml_to_value(v));
            }
            Value::Object(m)
        }
    }
}

fn yaml_to_value(v: &serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if n.is_f64() {
                if let Some(f) = n.as_f64() {
                    return Value::String(float_str(f));
                }
            }
            if let Some(i) = n.as_i64() {
                return Value::Number(i.into());
            }
            if let Some(u) = n.as_u64() {
                return Value::Number(u.into());
            }
            Value::String(n.to_string())
        }
        serde_yaml::Value::String(s) => Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_value).collect()),
        serde_yaml::Value::Mapping(m) => {
            let mut pairs: Vec<(String, Value)> = m
                .iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_value(v)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = Map::new();
            for (k, v) in pairs {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(t) => obj([
            ("tag", Value::String(t.tag.to_string())),
            ("value", yaml_to_value(&t.value)),
        ]),
    }
}

fn yaml_key_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        other => serde_yaml::to_string(other).unwrap_or_default(),
    }
}

/// Read a previously written cache hash from the output directory.
pub fn read_cache(out_dir: &Utf8Path) -> Option<String> {
    std::fs::read_to_string(out_dir.join(CACHE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write the hash to the cache file in the output directory atomically.
///
/// The hash is first written to a uniquely-named temp file
/// (`.weaveffi-cache.tmp.{pid}.{nanos}`) and then renamed onto the final cache
/// path. `std::fs::rename` is atomic on POSIX and uses `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING` on Windows, so concurrent readers always
/// observe either the previous cache contents or the fully written new
/// contents, never a partially written file.
pub fn write_cache(out_dir: &Utf8Path, hash: &str) -> Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let tmp_path = out_dir.join(format!("{CACHE_FILE}.tmp.{pid}.{nanos}"));
    let final_path = out_dir.join(CACHE_FILE);

    std::fs::write(tmp_path.as_std_path(), hash)?;
    if let Err(e) = std::fs::rename(tmp_path.as_std_path(), final_path.as_std_path()) {
        let _ = std::fs::remove_file(tmp_path.as_std_path());
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{Generator, Orchestrator};
    use crate::config::GeneratorConfig;
    use std::collections::HashMap;
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
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
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
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
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
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
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

    #[test]
    fn hash_invariant_under_hashmap_iteration_order() {
        // Populate the generators HashMap in two different insertion orders;
        // a naive `serde_json::to_string` might serialise them differently
        // depending on the random hasher, but the canonical form must not.
        let keys = [
            ("swift", "config_a"),
            ("android", "config_b"),
            ("kotlin", "config_c"),
            ("node", "config_d"),
            ("python", "config_e"),
        ];

        let mut gens_a: HashMap<String, toml::Value> = HashMap::new();
        for (k, v) in keys.iter() {
            gens_a.insert((*k).to_string(), toml::Value::String((*v).to_string()));
        }

        let mut gens_b: HashMap<String, toml::Value> = HashMap::new();
        for (k, v) in keys.iter().rev() {
            gens_b.insert((*k).to_string(), toml::Value::String((*v).to_string()));
        }

        let mut api_a = minimal_api();
        api_a.generators = Some(gens_a);
        let mut api_b = minimal_api();
        api_b.generators = Some(gens_b);

        assert_eq!(canonical_serialize(&api_a), canonical_serialize(&api_b));
        assert_eq!(hash_api(&api_a), hash_api(&api_b));
    }

    #[test]
    fn cache_write_is_atomic_under_concurrent_writers() {
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();

        // Each thread writes its own distinct SHA-256-shaped hash many times.
        // The atomic rename contract means the final file must end up equal
        // to exactly one of these hashes, never a truncated or interleaved
        // value.
        let thread_count = 16;
        let writes_per_thread = 50;
        let hashes: Vec<String> = (0..thread_count).map(|i| format!("{i:064x}")).collect();

        let mut handles = Vec::with_capacity(thread_count);
        for hash in &hashes {
            let hash = hash.clone();
            let dir = dir_path.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..writes_per_thread {
                    // A rare temp-name collision can make a single write fail;
                    // that's acceptable. The invariant we assert below is that
                    // the final cache file is never corrupt.
                    let _ = write_cache(&dir, &hash);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Exactly one hash string should be present (no interleaving, no
        // partial writes). Also verify no tmp files were left behind.
        let final_content = read_cache(&dir_path).expect("cache file should exist");
        assert!(
            hashes.contains(&final_content),
            "cache file is corrupted or contains an unexpected value: {final_content:?}"
        );

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir_path.as_std_path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{CACHE_FILE}.tmp."))
            })
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "no temp files should remain after concurrent writes: {leftover_tmp:?}"
        );
    }

    #[test]
    fn hash_stable_across_serde_versions() {
        // Known-fixture test: the hex digest of `minimal_api()` must not change
        // unless the canonical form itself is intentionally updated. This guards
        // against accidental drift in field ordering or float formatting across
        // serde / serde_json upgrades.
        let hash = hash_api(&minimal_api());
        assert_eq!(
            hash, "d7b4b3f85d86a0e1e09a9a661c2b80730526559273f8686e5d365f22dc9f80d3",
            "canonical form changed; update the expected hash only if intentional"
        );
    }
}
